use std::path::Path;

use anyhow::{Context, bail};

use crate::contract::{Manifest, Summary};
use crate::util;

pub fn generate(run_dir: &Path, manifest: &Manifest, summary: &Summary) -> anyhow::Result<()> {
    let manifest_json = escape_script_json(&serde_json::to_string(manifest)?);
    let summary_json = escape_script_json(&serde_json::to_string(summary)?);
    let flamegraph = if run_dir.join("report/assets/cpu-flamegraph.svg").is_file() {
        r#"<section><h2>CPU flame graph</h2><object class="flamegraph" data="assets/cpu-flamegraph.svg" type="image/svg+xml"></object></section>"#
    } else {
        ""
    };
    let profile_links = profile_links(run_dir)?;
    let html = REPORT_TEMPLATE
        .replace("__MANIFEST__", &manifest_json)
        .replace("__SUMMARY__", &summary_json)
        .replace("__FLAMEGRAPH__", flamegraph)
        .replace("__PROFILE_LINKS__", &profile_links);
    util::atomic_write(&run_dir.join("report/index.html"), html.as_bytes())?;
    Ok(())
}

pub fn open(run_dir: &Path) -> anyhow::Result<u8> {
    let report = run_dir.join("report/index.html");
    if !report.is_file() {
        bail!(
            "report does not exist at {}; run `v8scope analyze {}`",
            report.display(),
            run_dir.display()
        );
    }
    webbrowser::open(&format!(
        "file://{}",
        report.canonicalize()?.to_string_lossy()
    ))
    .context("failed to open default browser")?;
    Ok(0)
}

fn escape_script_json(value: &str) -> String {
    value
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
}

fn profile_links(run_dir: &Path) -> anyhow::Result<String> {
    let mut links = Vec::new();
    for relative in util::collect_files(&run_dir.join("profiles"))? {
        let extension = relative.extension().and_then(|value| value.to_str());
        if !matches!(
            extension,
            Some("cpuprofile" | "heapprofile" | "heapsnapshot")
        ) {
            continue;
        }
        let path = relative.to_string_lossy().replace('\\', "/");
        let label = relative
            .file_name()
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.clone());
        links.push(format!(
            "<a href=\"../profiles/{}\">{}</a>",
            escape_html(&path),
            escape_html(&label)
        ));
    }
    links.push("<a href=\"../summary.json\">summary.json</a>".into());
    links.push("<a href=\"../manifest.json\">manifest.json</a>".into());
    Ok(links.join(" · "))
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

const REPORT_TEMPLATE: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>V8Scope Report</title>
<style>
:root{color-scheme:light dark;--bg:#0c0e12;--panel:#151923;--text:#eef1f7;--muted:#929aab;--line:#293042;--accent:#8bd5ca;--warn:#f5a97f;--critical:#ed8796}*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--text);font:14px/1.45 ui-monospace,SFMono-Regular,Menlo,monospace}main{width:min(1180px,calc(100% - 40px));margin:36px auto 80px}header{display:flex;justify-content:space-between;align-items:end;margin-bottom:28px}h1{font:700 30px/1.1 ui-sans-serif,system-ui;margin:0}h2{font:650 16px/1.2 ui-sans-serif,system-ui;margin:0 0 16px}.muted{color:var(--muted)}.grid{display:grid;grid-template-columns:repeat(4,1fr);gap:12px;margin:16px 0 28px}.card,section{background:var(--panel);border:1px solid var(--line);border-radius:12px}.card{padding:16px}.value{font-size:24px;margin-top:8px}section{padding:20px;margin:12px 0}.flamegraph{width:100%;height:620px;border:0;background:#fff}.async-layout{display:grid;grid-template-columns:minmax(360px,1fr) minmax(420px,1.2fr);gap:18px}.async-graph{width:100%;min-height:420px;border:1px solid var(--line);border-radius:10px}.async-edge{stroke:var(--muted);stroke-width:1.5;opacity:.5}.async-node circle{fill:var(--panel);stroke:var(--accent);stroke-width:2}.async-node text{fill:var(--text);font-size:11px;text-anchor:middle}.async-node.active circle,.async-node:hover circle{fill:var(--accent)}.async-node.active text,.async-node:hover text{fill:var(--bg)}table{width:100%;border-collapse:collapse}th,td{text-align:left;padding:9px 8px;border-bottom:1px solid var(--line)}th{color:var(--muted);font-weight:500}.finding{border-left:3px solid var(--warn);padding:2px 0 2px 14px;margin:14px 0}.finding.critical{border-color:var(--critical)}code{color:var(--accent)}a{color:var(--accent)}@media(max-width:800px){.grid{grid-template-columns:1fr 1fr}header{display:block}.value{font-size:20px}.async-layout{grid-template-columns:1fr}}
</style>
</head>
<body><main><header><div><h1>V8Scope</h1><div class="muted" id="run"></div></div><div class="muted" id="runtime"></div></header><div class="grid" id="metrics"></div><section><h2>Findings</h2><div id="findings"></div></section>__FLAMEGRAPH__<section><h2>CPU hotspots</h2><table><thead><tr><th>Function</th><th>Location</th><th>Self</th><th>Total</th></tr></thead><tbody id="cpu"></tbody></table></section><section><h2>Allocation hotspots</h2><table><thead><tr><th>Function</th><th>Location</th><th>Self</th><th>Total</th></tr></thead><tbody id="heap"></tbody></table></section><section id="async-section"><h2>Async topology</h2><div class="async-layout"><svg id="async-graph" class="async-graph" viewBox="0 0 600 420" role="img" aria-label="Async causal topology"></svg><table><thead><tr><th>Type</th><th>Resources</th><th>Wait p95</th><th>Callback total</th></tr></thead><tbody id="async-types"></tbody></table></div><h2>Slow async operations</h2><table><thead><tr><th>Type</th><th>Wait</th><th>Callback</th><th>Causal chain</th><th>Origin</th></tr></thead><tbody id="async"></tbody></table></section><section><h2>Raw profiles</h2><div>__PROFILE_LINKS__</div></section></main>
<script>const manifest=__MANIFEST__;const summary=__SUMMARY__;const $=s=>document.querySelector(s);const esc=s=>String(s??'').replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));const bytes=n=>{const u=['B','KiB','MiB','GiB'];let i=0;while(n>=1024&&i<u.length-1){n/=1024;i++}return`${n.toFixed(i?1:0)} ${u[i]}`};$('#run').textContent=`${manifest.name} · ${manifest.mode} · ${summary.duration_ms.toFixed(0)} ms`;$('#runtime').textContent=`${manifest.runtime.node??'Node unknown'} · V8 ${manifest.runtime.v8??'unknown'} · ${manifest.platform.os}/${manifest.platform.arch}`;const cards=[['EL p99',`${summary.event_loop.delay_p99_ms.toFixed(2)} ms`],['EL utilization',`${(summary.event_loop.utilization_avg*100).toFixed(1)}%`],['Peak RSS',bytes(summary.memory.rss_max_bytes)],['GC blocking/s',`${summary.gc.max_blocking_ms_per_second.toFixed(2)} ms`]];$('#metrics').innerHTML=cards.map(([k,v])=>`<div class="card"><div class="muted">${esc(k)}</div><div class="value">${esc(v)}</div></div>`).join('');$('#findings').innerHTML=summary.findings.length?summary.findings.map(f=>`<div class="finding ${f.severity}"><strong>${esc(f.title)}</strong><div class="muted">${esc(f.recommendation)}</div></div>`).join(''):'<div class="muted">No threshold violations detected.</div>';const rows=(items,format)=>items.slice(0,30).map(h=>`<tr><td>${esc(h.function)}</td><td><code>${esc(h.url)}:${h.line}</code></td><td>${format(h.self_value)}</td><td>${format(h.total_value)}</td></tr>`).join('');$('#cpu').innerHTML=rows(summary.cpu.hotspots,n=>`${n.toFixed(2)} ms`);$('#heap').innerHTML=rows(summary.memory.allocation_hotspots,bytes);if(summary.asynchronous.enabled){const topology=Object.entries(summary.asynchronous.topology);$('#async-types').innerHTML=topology.map(([type,v])=>`<tr><td>${esc(type)}</td><td>${v.resources}</td><td>${v.wait_p95_ms.toFixed(3)} ms</td><td>${v.total_callback_ms.toFixed(3)} ms</td></tr>`).join('');$('#async').innerHTML=summary.asynchronous.slow_callbacks.slice(0,30).map(c=>`<tr><td>${esc(c.resource_type)}</td><td>${c.wait_ms.toFixed(3)} ms</td><td>${c.duration_ms.toFixed(3)} ms</td><td>${esc(c.causal_chain.join(' → '))}</td><td><code>${esc(c.stack[0]??'')}</code></td></tr>`).join('');const svg=$('#async-graph'),ns='http://www.w3.org/2000/svg',positions=new Map();topology.forEach(([type],i)=>{const a=2*Math.PI*i/Math.max(1,topology.length)-Math.PI/2;positions.set(type,[300+190*Math.cos(a),210+150*Math.sin(a)])});Object.entries(summary.asynchronous.causal_edges).forEach(([edge,count])=>{const [from,to]=edge.split(' -> '),a=positions.get(from),b=positions.get(to);if(!a||!b)return;const line=document.createElementNS(ns,'line');line.setAttribute('x1',a[0]);line.setAttribute('y1',a[1]);line.setAttribute('x2',b[0]);line.setAttribute('y2',b[1]);line.setAttribute('class','async-edge');line.dataset.from=from;line.dataset.to=to;const title=document.createElementNS(ns,'title');title.textContent=`${edge}: ${count}`;line.appendChild(title);svg.appendChild(line)});topology.forEach(([type,v])=>{const [x,y]=positions.get(type),g=document.createElementNS(ns,'g');g.setAttribute('class','async-node');g.setAttribute('transform',`translate(${x} ${y})`);g.dataset.type=type;const circle=document.createElementNS(ns,'circle');circle.setAttribute('r',String(22+Math.min(20,Math.sqrt(v.resources)*3)));const label=document.createElementNS(ns,'text');label.setAttribute('y','4');label.textContent=type.length>14?`${type.slice(0,12)}…`:type;const title=document.createElementNS(ns,'title');title.textContent=`${type}: ${v.resources} resources, p95 wait ${v.wait_p95_ms.toFixed(3)} ms`;g.append(circle,label,title);g.addEventListener('click',()=>{svg.querySelectorAll('.async-node').forEach(n=>n.classList.toggle('active',n.dataset.type===type));svg.querySelectorAll('.async-edge').forEach(e=>e.style.opacity=(e.dataset.from===type||e.dataset.to===type)?'1':'.12')});svg.appendChild(g)})}else{$('#async-section').remove()}</script></body></html>"#;
