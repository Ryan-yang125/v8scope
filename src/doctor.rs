#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuAssessment {
    Data,
    Performance,
    None,
}

/// Reproduces Clinic Doctor's two-state continuous HMM decision boundary.
/// Input values are process CPU ratios where 1.0 means one fully utilized core.
pub fn assess_cpu(observations: &[f64]) -> CpuAssessment {
    if observations.len() < 4 || observations.iter().any(|value| !value.is_finite()) {
        return CpuAssessment::Data;
    }
    let overall_p90 = percentile(observations, 0.90);
    let Some(model) = GaussianHmm::fit(observations) else {
        return CpuAssessment::Data;
    };
    let states = model.viterbi(observations);
    let mut groups = [Vec::new(), Vec::new()];
    for (value, state) in observations.iter().zip(states) {
        groups[state].push(*value);
    }
    if groups[0].len() <= 1 || groups[1].len() <= 1 {
        return performance(overall_p90 < 0.90);
    }
    let means = [mean(&groups[0]), mean(&groups[1])];
    let deviations = [sample_deviation(&groups[0]), sample_deviation(&groups[1])];
    let common_deviation = 2.0 * (deviations[0] + deviations[1]);
    let separation = if common_deviation > f64::EPSILON {
        (means[0] - means[1]) / common_deviation
    } else {
        f64::INFINITY.copysign(means[0] - means[1])
    };
    if separation.abs() < 1.0 {
        return performance(overall_p90 < 0.90);
    }
    let application = if means[0] < means[1] {
        &groups[0]
    } else {
        &groups[1]
    };
    performance(percentile(application, 0.90) < 0.90)
}

fn performance(value: bool) -> CpuAssessment {
    if value {
        CpuAssessment::Performance
    } else {
        CpuAssessment::None
    }
}

#[derive(Clone)]
struct GaussianHmm {
    initial: [f64; 2],
    transition: [[f64; 2]; 2],
    means: [f64; 2],
    variances: [f64; 2],
}

impl GaussianHmm {
    fn fit(values: &[f64]) -> Option<Self> {
        let mut sorted = values.to_vec();
        sorted.sort_by(f64::total_cmp);
        let variance = population_variance(values).max(1e-6);
        let mut model = Self {
            initial: [0.5, 0.5],
            transition: [[0.90, 0.10], [0.10, 0.90]],
            means: [percentile(&sorted, 0.25), percentile(&sorted, 0.75)],
            variances: [variance, variance],
        };
        let mut previous_likelihood = f64::NEG_INFINITY;
        let mut converged = false;
        for _ in 0..200 {
            let (alpha, scales, likelihood) = model.forward(values)?;
            let beta = model.backward(values, &scales);
            let length = values.len();
            let mut gamma = vec![[0.0; 2]; length];
            for time in 0..length {
                let denominator = (0..2)
                    .map(|state| alpha[time][state] * beta[time][state])
                    .sum::<f64>();
                if denominator <= f64::MIN_POSITIVE {
                    return None;
                }
                for state in 0..2 {
                    gamma[time][state] = alpha[time][state] * beta[time][state] / denominator;
                }
            }
            let mut transition_numerator = [[0.0; 2]; 2];
            let mut transition_denominator = [0.0; 2];
            for time in 0..length - 1 {
                let mut denominator = 0.0;
                let mut xi = [[0.0; 2]; 2];
                for from in 0..2 {
                    for to in 0..2 {
                        xi[from][to] = alpha[time][from]
                            * model.transition[from][to]
                            * model.emission(to, values[time + 1])
                            * beta[time + 1][to];
                        denominator += xi[from][to];
                    }
                }
                if denominator <= f64::MIN_POSITIVE {
                    return None;
                }
                for from in 0..2 {
                    transition_denominator[from] += gamma[time][from];
                    for to in 0..2 {
                        transition_numerator[from][to] += xi[from][to] / denominator;
                    }
                }
            }
            model.initial = gamma[0];
            for state in 0..2 {
                let weight = gamma.iter().map(|row| row[state]).sum::<f64>();
                if weight <= 1e-9 {
                    return None;
                }
                model.means[state] = values
                    .iter()
                    .zip(&gamma)
                    .map(|(value, row)| value * row[state])
                    .sum::<f64>()
                    / weight;
                model.variances[state] = values
                    .iter()
                    .zip(&gamma)
                    .map(|(value, row)| row[state] * (value - model.means[state]).powi(2))
                    .sum::<f64>()
                    / weight;
                model.variances[state] = model.variances[state].max(1e-8);
                if transition_denominator[state] > 1e-9 {
                    for (next, numerator) in transition_numerator[state].iter().enumerate() {
                        model.transition[state][next] = *numerator / transition_denominator[state];
                    }
                }
            }
            if (likelihood - previous_likelihood).abs() < 0.001 {
                converged = true;
                break;
            }
            previous_likelihood = likelihood;
        }
        converged.then_some(model)
    }

    fn forward(&self, values: &[f64]) -> Option<(Vec<[f64; 2]>, Vec<f64>, f64)> {
        let mut alpha = vec![[0.0; 2]; values.len()];
        let mut scales = vec![0.0; values.len()];
        for (state, value) in alpha[0].iter_mut().enumerate() {
            *value = self.initial[state] * self.emission(state, values[0]);
        }
        normalize(&mut alpha[0], &mut scales[0])?;
        for time in 1..values.len() {
            for state in 0..2 {
                alpha[time][state] = (0..2)
                    .map(|previous| alpha[time - 1][previous] * self.transition[previous][state])
                    .sum::<f64>()
                    * self.emission(state, values[time]);
            }
            let (before, rest) = alpha.split_at_mut(time);
            let _ = before;
            normalize(&mut rest[0], &mut scales[time])?;
        }
        let likelihood = scales.iter().map(|scale| scale.ln()).sum();
        Some((alpha, scales, likelihood))
    }

    fn backward(&self, values: &[f64], scales: &[f64]) -> Vec<[f64; 2]> {
        let mut beta = vec![[1.0; 2]; values.len()];
        for time in (0..values.len() - 1).rev() {
            for state in 0..2 {
                beta[time][state] = (0..2)
                    .map(|next| {
                        self.transition[state][next]
                            * self.emission(next, values[time + 1])
                            * beta[time + 1][next]
                    })
                    .sum::<f64>()
                    / scales[time + 1].max(f64::MIN_POSITIVE);
            }
        }
        beta
    }

    fn viterbi(&self, values: &[f64]) -> Vec<usize> {
        let mut score = vec![[f64::NEG_INFINITY; 2]; values.len()];
        let mut previous = vec![[0usize; 2]; values.len()];
        for (state, value) in score[0].iter_mut().enumerate() {
            *value = self.initial[state].max(f64::MIN_POSITIVE).ln()
                + self.emission(state, values[0]).ln();
        }
        for time in 1..values.len() {
            for state in 0..2 {
                let candidates = [
                    score[time - 1][0] + self.transition[0][state].max(f64::MIN_POSITIVE).ln(),
                    score[time - 1][1] + self.transition[1][state].max(f64::MIN_POSITIVE).ln(),
                ];
                previous[time][state] = usize::from(candidates[1] > candidates[0]);
                score[time][state] =
                    candidates[previous[time][state]] + self.emission(state, values[time]).ln();
            }
        }
        let mut states = vec![0usize; values.len()];
        states[values.len() - 1] =
            usize::from(score[values.len() - 1][1] > score[values.len() - 1][0]);
        for time in (1..values.len()).rev() {
            states[time - 1] = previous[time][states[time]];
        }
        states
    }

    fn emission(&self, state: usize, value: f64) -> f64 {
        let variance = self.variances[state].max(1e-8);
        let exponent = -((value - self.means[state]).powi(2)) / (2.0 * variance);
        (exponent.exp() / (2.0 * std::f64::consts::PI * variance).sqrt()).max(1e-300)
    }
}

fn normalize(values: &mut [f64; 2], scale: &mut f64) -> Option<()> {
    *scale = values.iter().sum();
    if !scale.is_finite() || *scale <= f64::MIN_POSITIVE {
        return None;
    }
    for value in values {
        *value /= *scale;
    }
    Some(())
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

fn population_variance(values: &[f64]) -> f64 {
    let average = mean(values);
    values
        .iter()
        .map(|value| (value - average).powi(2))
        .sum::<f64>()
        / values.len() as f64
}

fn sample_deviation(values: &[f64]) -> f64 {
    if values.len() <= 1 {
        return 0.0;
    }
    let average = mean(values);
    (values
        .iter()
        .map(|value| (value - average).powi(2))
        .sum::<f64>()
        / (values.len() - 1) as f64)
        .sqrt()
}

fn percentile(values: &[f64], quantile: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut values = values.to_vec();
    values.sort_by(f64::total_cmp);
    let index = (quantile.clamp(0.0, 1.0) * (values.len() - 1) as f64).round() as usize;
    values[index]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noisy_ratios(values: &[f64], level: f64) -> Vec<f64> {
        let mut state = 294_915_u64;
        values
            .iter()
            .map(|value| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                let noise = ((state >> 33) as f64 / u32::MAX as f64) * level;
                (value + noise) * 0.01
            })
            .collect()
    }

    #[test]
    fn matches_clinic_doctor_one_mode_vectors() {
        for noise in [0.0, 0.1, 0.3, 0.5] {
            assert_eq!(
                assess_cpu(&noisy_ratios(
                    &[100., 100., 120., 100., 110., 100., 100., 110., 90., 110.],
                    noise,
                )),
                CpuAssessment::None
            );
            assert_eq!(
                assess_cpu(&noisy_ratios(
                    &[50., 40., 10., 10., 80., 50., 40., 1., 10., 30., 10.],
                    noise,
                )),
                CpuAssessment::Performance
            );
        }
    }

    #[test]
    fn matches_clinic_doctor_two_mode_vectors() {
        for noise in [0.0, 0.1, 0.3, 0.5] {
            assert_eq!(
                assess_cpu(&noisy_ratios(
                    &[200., 200., 100., 90., 190., 200., 80., 110., 190., 200.],
                    noise,
                )),
                CpuAssessment::None
            );
            assert_eq!(
                assess_cpu(&noisy_ratios(
                    &[200., 200., 15., 10., 190., 200., 5., 15., 190., 200.],
                    noise,
                )),
                CpuAssessment::Performance
            );
        }
    }

    #[test]
    fn matches_clinic_doctor_opposite_and_small_cluster_vectors() {
        let good_opposite = [
            200., 200., 100., 90., 190., 200., 80., 110., 190., 200., 200., 200., 100., 90., 190.,
            200., 80., 110., 190., 200.,
        ];
        let bad_opposite = [
            200., 200., 15., 10., 190., 200., 5., 15., 190., 200., 200., 200., 15., 10., 190.,
            200., 5., 15., 190., 200.,
        ];
        for noise in [0.0, 0.1, 0.3, 0.5] {
            assert_eq!(
                assess_cpu(&noisy_ratios(&good_opposite, noise)),
                CpuAssessment::None
            );
            assert_eq!(
                assess_cpu(&noisy_ratios(&bad_opposite, noise)),
                CpuAssessment::Performance
            );
            assert_eq!(
                assess_cpu(&noisy_ratios(
                    &[200., 200., 100., 90., 190., 200., 80., 110., 190., 0.],
                    noise,
                )),
                CpuAssessment::None
            );
            assert_eq!(
                assess_cpu(&noisy_ratios(
                    &[50., 40., 10., 10., 200., 50., 40., 10., 10., 30., 10.],
                    noise,
                )),
                CpuAssessment::Performance
            );
        }
    }

    #[test]
    fn rejects_too_little_data() {
        assert_eq!(assess_cpu(&[1.0, 1.0, 1.0]), CpuAssessment::Data);
    }
}
