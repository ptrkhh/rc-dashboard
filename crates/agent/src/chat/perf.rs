//! Measurements for the spec's Performance table (Part 4, Task 23). Each test
//! is `#[ignore]` and prints one line; run them one at a time, on a private
//! tmux server:
//! `env -u TMUX TMUX_TMPDIR=$(mktemp -d) cargo test -p cdash-agent --release chat::perf -- --ignored --nocapture --test-threads=1`

use super::{claude, hub::Hub, notify};
use crate::chat::claude::tests::{fixture_lines, fixture_with};
use crate::chat::tmux_io::tests::tempfile_path;
use crate::collect::ctx::Ctx;
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn rss_bytes() -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap();
    let kb = s.lines().find_map(|l| l.strip_prefix("VmRSS:")).unwrap().trim().trim_end_matches("kB").trim();
    kb.parse::<u64>().unwrap() * 1024
}

fn mb(b: u64) -> f64 {
    b as f64 / 1_048_576.0
}

/// CPU seconds this process has used: user + system (own threads only).
fn cpu_secs() -> f64 {
    let s = std::fs::read_to_string("/proc/self/stat").unwrap();
    // Fields after the `(comm)` one; utime and stime are the 12th and 13th of them.
    let f: Vec<&str> = s[s.rfind(')').unwrap() + 2..].split(' ').collect();
    let ticks = f[11].parse::<f64>().unwrap() + f[12].parse::<f64>().unwrap();
    ticks / unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64
}

/// Start a Claude hub on `pid` and wait until it has replayed its history.
async fn hub_on(ctx: &Arc<Ctx>, name: &str, pid: i32) -> Hub {
    let (engine, rx) = claude::start(Arc::clone(ctx), name.to_string(), pid);
    let hub = Hub::spawn("claude", engine, rx, Arc::clone(&ctx.host.log), name.to_string());
    assert!(hub.subscribe(None, None).await.is_some(), "hub {name} ended");
    hub
}

fn session_file(dir: &Path, pid: i32, sid: &str, cwd: &str, status: &str) {
    std::fs::write(dir.join("sessions").join(format!("{pid}.json")), json!({"sessionId":sid,"cwd":cwd,"status":status}).to_string()).unwrap();
}

/// 10,000 turns of five records: a prompt, a Bash call, its result, text, `turn_duration`.
fn write_synthetic(path: &Path, turns: usize) {
    let mut w = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    let (prompt, out, text) = ("p".repeat(80), "o".repeat(1000), "t".repeat(400));
    let mut parent = "root".to_string();
    let mut line = |w: &mut std::io::BufWriter<std::fs::File>, id: String, mut v: Value| {
        v["uuid"] = json!(id);
        v["parentUuid"] = json!(parent);
        v["isSidechain"] = json!(false);
        writeln!(w, "{v}").unwrap();
        parent = id;
    };
    for i in 0..turns {
        let tool = format!("toolu_{i}");
        line(&mut w, format!("u{i}"), json!({"type":"user","promptSource":"typed","message":{"role":"user","content":prompt}}));
        line(&mut w, format!("a{i}"), json!({"type":"assistant","message":{"id":format!("m{i}a"),"role":"assistant","content":[{"type":"tool_use","id":tool,"name":"Bash","input":{"command":"npm test"}}]}}));
        line(&mut w, format!("r{i}"), json!({"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":tool,"content":[{"type":"text","text":out}],"is_error":false}]}}));
        line(&mut w, format!("t{i}"), json!({"type":"assistant","message":{"id":format!("m{i}b"),"role":"assistant","content":[{"type":"text","text":text}]}}));
        line(&mut w, format!("d{i}"), json!({"type":"system","subtype":"turn_duration","durationMs":1000}));
    }
    w.flush().unwrap();
}

/// A tmux server that is not the owner's: `TMUX_TMPDIR` is private and `TMUX` unset.
fn private_tmux() {
    assert!(std::env::var_os("TMUX").is_none(), "run with `env -u TMUX`");
    let t = std::env::var("TMUX_TMPDIR").unwrap_or_default();
    assert!(!t.is_empty() && t != "/tmp", "run with a private TMUX_TMPDIR");
}

fn tmux(args: &[&str]) -> String {
    #[allow(clippy::disallowed_types)] // test-only: a private tmux server, outside any Runner
    let o = std::process::Command::new("tmux").args(args).output().unwrap();
    String::from_utf8_lossy(&o.stdout).into_owned()
}

struct KillServer;
impl Drop for KillServer {
    fn drop(&mut self) {
        tmux(&["kill-server"]);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "measurement"]
async fn perf_memory_per_hub() {
    let (ctx, dir) = fixture_with("perf-mem", "");
    std::fs::create_dir_all(dir.join("projects/-perf")).unwrap();
    // One hub over 10,000 turns x 5 events = 50,000 events.
    let synth = dir.join("projects/-perf/synthetic.jsonl");
    write_synthetic(&synth, 10_000);
    session_file(&dir, 300_000, "synthetic", "/perf", "idle");
    let before = rss_bytes();
    let _one = hub_on(&ctx, "cdash-perf-synth", 300_000).await;
    let synth_mb = mb(rss_bytes().saturating_sub(before));
    // Hubs over the owner's 10 most recently modified transcripts, read in place.
    let home = PathBuf::from(std::env::var("HOME").unwrap()).join(".claude/projects");
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(home)
        .unwrap()
        .flatten()
        .flat_map(|d| std::fs::read_dir(d.path()).into_iter().flatten().flatten())
        .map(|f| f.path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .filter_map(|p| Some((p.metadata().ok()?.modified().ok()?, p)))
        .collect();
    found.sort_by_key(|f| std::cmp::Reverse(f.0));
    let (mut total, mut largest, mut bytes, mut keep) = (0u64, 0u64, 0u64, Vec::new());
    for (i, (_, p)) in found.iter().take(10).enumerate() {
        let sid = format!("real-{i}");
        std::os::unix::fs::symlink(p, dir.join(format!("projects/-perf/{sid}.jsonl"))).unwrap();
        session_file(&dir, 300_001 + i as i32, &sid, "/perf", "idle");
        bytes += p.metadata().unwrap().len();
        let b = rss_bytes();
        keep.push(hub_on(&ctx, &format!("cdash-perf-real-{i}"), 300_001 + i as i32).await);
        let d = rss_bytes().saturating_sub(b);
        total += d;
        largest = largest.max(d);
    }
    println!(
        "perf_memory_per_hub: synthetic 50,000 events {synth_mb:.1} MB; {} real hubs over {:.1} MB of transcript: total {:.1} MB, largest {:.1} MB",
        keep.len(), mb(bytes), mb(total), mb(largest)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "measurement"]
async fn perf_idle_cpu() {
    let (ctx, dir) = fixture_with("perf-cpu", "");
    let mut keep = Vec::new();
    for i in 0..10 {
        let (pid, sid) = (310_000 + i, format!("idle-{i}"));
        crate::chat::claude::tests::write_transcript(&dir, &sid, &fixture_lines(1..14));
        session_file(&dir, pid, &sid, "/proj", "idle");
        keep.push(hub_on(&ctx, &format!("cdash-perf-{i}"), pid).await);
    }
    tokio::time::sleep(Duration::from_secs(5)).await;
    let (c0, t0) = (cpu_secs(), Instant::now());
    tokio::time::sleep(Duration::from_secs(60)).await;
    let pct = (cpu_secs() - c0) / t0.elapsed().as_secs_f64() * 100.0;
    println!("perf_idle_cpu: {} idle Claude hubs, 60 s: {pct:.2}% of one core", keep.len());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "measurement"]
async fn perf_hub_for_latency() {
    private_tmux();
    let _kill = KillServer;
    let dir = tempfile_path("perf-lat");
    std::fs::create_dir_all(dir.join("sessions")).unwrap();
    std::fs::create_dir_all(dir.join("projects/-perf")).unwrap();
    for i in 0..11 {
        let name = format!("cdash-perf-{i}");
        tmux(&["new-session", "-d", "-s", &name, "sleep 600"]);
        let pid: i32 = tmux(&["display-message", "-p", "-t", &format!("={name}:"), "#{pane_pid}"]).trim().parse().unwrap();
        let sid = format!("lat-{i}");
        std::fs::write(dir.join(format!("projects/-perf/{sid}.jsonl")), fixture_lines(1..14)).unwrap();
        session_file(&dir, pid, &sid, "/perf", "idle");
    }
    let ctx = crate::chat::claude::tests::ctx_with_path(dir, std::env::var("PATH").unwrap());
    super::hub_for(&ctx, "cdash-perf-0").await.unwrap();
    let c = Arc::clone(&ctx);
    let creating = tokio::spawn(async move { notify::ensure_hubs(&c).await });
    let mut ms = Vec::new();
    for _ in 0..50 {
        let t = Instant::now();
        super::hub_for(&ctx, "cdash-perf-0").await.unwrap();
        ms.push(t.elapsed().as_secs_f64() * 1000.0);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    creating.await.unwrap();
    let made = ctx.chats.lock().await.len();
    ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!("perf_hub_for_latency: {made} hubs; 50 calls, median {:.1} ms, p95 {:.1} ms, max {:.1} ms", ms[25], ms[47], ms[49]);
}
