// tcp_speed_tester.rs
//
// 依赖：anyhow chrono csv serde serde_derive tokio trust-dns-resolver toml
// 建议：cargo add anyhow chrono csv serde serde_derive tokio trust-dns-resolver --features=tokio-runtime
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    net::{IpAddr, SocketAddr},
    path::Path,
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize};
use tokio::{net::TcpStream, time::sleep};
use trust_dns_resolver::{
    config::{ResolverConfig, ResolverOpts},
    TokioAsyncResolver,
};

/* ---------- 数据结构 ---------- */

#[derive(Debug, Serialize, Deserialize, Clone)]
struct SpeedTestRecord {
    exchange: String,
    endpoint: String, // host:port
    domain: String,
    port: u16,
    ip: String,
    timestamp: DateTime<Utc>,
    connection_time_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SpeedTestTarget {
    exchange: String,
    endpoint: String,
}

#[derive(Deserialize)]
struct Config {
    targets: Vec<SpeedTestTarget>,
}

/* ---------- 常量 ---------- */

const CONFIG_PATH: &str = "targets.toml";
const RESULTS_PATH: &str = "tcp_speed_results.csv";

const HOSTS_PATH: &str = "/etc/hosts";
const HOSTS_MARKER_START: &str = "# TCP Speed Test";
const HOSTS_MARKER_END: &str = "# End TCP Speed Test";

const TEST_ATTEMPTS: usize = 5;
const CONNECTION_TIMEOUT_SECS: u64 = 5;
const TEST_INTERVAL_MINS: u64 = 5;
const RETENTION_HOURS: i64 = 10;

/* ---------- 主入口 ---------- */

#[tokio::main]
async fn main() -> Result<()> {
    println!(
        "TCP Speed Tester 启动：每 {TEST_INTERVAL_MINS} 分钟测速一次，结果保存 {RESULTS_PATH}"
    );

    let targets = load_targets(CONFIG_PATH)?;
    if targets.is_empty() {
        println!("配置文件 {CONFIG_PATH} 里没有测速目标，退出。");
        return Ok(());
    }

    let resolver = Arc::new(TokioAsyncResolver::tokio(
        ResolverConfig::default(),
        ResolverOpts::default(),
    )?);

    loop {
        let t0 = Local::now();
        println!("开始测速 {}", t0);

        if let Ok(rs) = run_speed_tests(&targets, resolver.clone()).await {
            if !rs.is_empty() {
                save_results(&rs).await?;
                cleanup_old_results().await?;
                update_hosts_file().await?;
            }
        }

        let used = Local::now().signed_duration_since(t0).num_seconds();
        let wait = (TEST_INTERVAL_MINS * 60) as i64 - used;
        if wait > 0 {
            println!("等待 {wait}s 开始下一轮……");
            sleep(Duration::from_secs(wait as u64)).await;
        }
    }
}

/* ---------- 公共 I/O ---------- */

fn load_targets(path: &str) -> Result<Vec<SpeedTestTarget>> {
    let txt = fs::read_to_string(path).with_context(|| format!("读取 {path} 失败"))?;
    let cfg: Config = toml::from_str(&txt).with_context(|| format!("解析 {path} 失败"))?;
    Ok(cfg.targets)
}

async fn save_results(rs: &[SpeedTestRecord]) -> Result<()> {
    let first = !Path::new(RESULTS_PATH).exists();
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(RESULTS_PATH)?;
    let mut w = csv::WriterBuilder::new()
        .has_headers(first)
        .from_writer(file);
    for r in rs {
        w.serialize(r)?;
    }
    w.flush()?;
    Ok(())
}

/* ---------- 速度测试 ---------- */

async fn try_connect(ip: IpAddr, port: u16) -> Result<f64> {
    let addr = SocketAddr::new(ip, port);
    let start = Instant::now();
    tokio::time::timeout(
        Duration::from_secs(CONNECTION_TIMEOUT_SECS),
        TcpStream::connect(addr),
    )
    .await
    .map_err(|_| anyhow!("超时"))?
    .map_err(|e| anyhow!(e))?;
    Ok(start.elapsed().as_secs_f64() * 1000.0)
}

async fn run_speed_tests(
    targets: &[SpeedTestTarget],
    resolver: Arc<TokioAsyncResolver>,
) -> Result<Vec<SpeedTestRecord>> {
    let mut out = Vec::new();

    for (idx, t) in targets.iter().enumerate() {
        println!("[{}/{}] {}", idx + 1, targets.len(), t.endpoint);

        let (domain, port) = t
            .endpoint
            .split_once(':')
            .ok_or_else(|| anyhow!("endpoint 格式错误 {}", t.endpoint))?;
        let port: u16 = port.parse()?;

        let ips: Vec<IpAddr> = resolver.lookup_ip(domain).await?.iter().collect();
        if ips.is_empty() {
            println!("  DNS 无结果，跳过");
            continue;
        }

        for ip in ips {
            let mut times = Vec::new();
            for _ in 0..TEST_ATTEMPTS {
                if let Ok(ms) = try_connect(ip, port).await {
                    times.push(ms);
                }
                sleep(Duration::from_millis(80)).await;
            }
            if !times.is_empty() {
                let avg = times.iter().sum::<f64>() / times.len() as f64;
                println!("  {ip} 平均 {:.2}ms", avg);
                out.push(SpeedTestRecord {
                    exchange: t.exchange.clone(),
                    endpoint: t.endpoint.clone(),
                    domain: domain.to_string(),
                    port,
                    ip: ip.to_string(),
                    timestamp: Utc::now(),
                    connection_time_ms: avg,
                });
            } else {
                println!("  {ip} 全部失败");
            }
        }
    }
    Ok(out)
}

/* ---------- 清理历史数据 ---------- */

async fn cleanup_old_results() -> Result<()> {
    if !Path::new(RESULTS_PATH).exists() {
        return Ok(());
    }
    let rdr = csv::Reader::from_path(RESULTS_PATH)?;
    let mut records: Vec<SpeedTestRecord> = rdr.into_deserialize().collect::<csv::Result<_>>()?;

    let before = records.len();
    let cutoff = Utc::now() - chrono::Duration::hours(RETENTION_HOURS);
    records.retain(|r| r.timestamp >= cutoff);
    if before == records.len() {
        return Ok(());
    }

    let file = File::create(RESULTS_PATH)?;
    let mut w = csv::Writer::from_writer(file);
    for r in records {
        w.serialize(r)?;
    }
    w.flush()?;
    Ok(())
}

/* ---------- hosts 更新 ---------- */

#[derive(Default)]
struct Stat {
    total: f64,
    cnt: usize,
}

async fn update_hosts_file() -> Result<()> {
    if !Path::new(RESULTS_PATH).exists() {
        println!("尚无测速记录，跳过 hosts 更新");
        return Ok(());
    }

    // 1. 统计每个 domain/ip 的平均延迟
    let rdr = csv::Reader::from_path(RESULTS_PATH)?;
    let mut stats: HashMap<(String, String, u16), Stat> = HashMap::new(); // key: (domain, ip, port)

    for rec in rdr.into_deserialize::<SpeedTestRecord>() {
        let r = rec?;
        let s = stats
            .entry((r.domain.clone(), r.ip.clone(), r.port))
            .or_default();
        s.total += r.connection_time_ms;
        s.cnt += 1;
    }

    if stats.is_empty() {
        println!("记录为空，跳过");
        return Ok(());
    }

    // 2. 为每个 domain 选择平均最低的 IP
    let mut best: HashMap<String, (String, u16, f64)> = HashMap::new(); // domain -> (ip, port, avg)
    for ((domain, ip, port), s) in &stats {
        let avg = s.total / s.cnt as f64;
        best.entry(domain.clone())
            .and_modify(|e| {
                if avg < e.2 {
                    *e = (ip.clone(), *port, avg)
                }
            })
            .or_insert((ip.clone(), *port, avg));
    }

    // 3. 再次验证可达性
    let mut ok_map = HashMap::<String, String>::new(); // domain -> ip
    let mut new_records = Vec::new();

    for (domain, (ip_str, port, _)) in best {
        let ip: IpAddr = ip_str.parse()?;
        match try_connect(ip, port).await {
            Ok(ms) => {
                println!("{domain} 验证 OK {ip} {:.2}ms", ms);
                ok_map.insert(domain.clone(), ip_str.clone());

                new_records.push(SpeedTestRecord {
                    exchange: String::new(),
                    endpoint: format!("{domain}:{port}"),
                    domain,
                    port,
                    ip: ip_str,
                    timestamp: Utc::now(),
                    connection_time_ms: ms,
                });
            }
            Err(e) => {
                println!("{domain} 验证失败 {ip} - {e}");
            }
        }
    }

    if ok_map.is_empty() {
        println!("没有可用 IP，hosts 不变");
        return Ok(());
    }

    update_linux_hosts(&ok_map)?;
    save_results(&new_records).await?;

    Ok(())
}

fn update_linux_hosts(best: &HashMap<String, String>) -> Result<()> {
    if !Path::new(HOSTS_PATH).exists() {
        return Err(anyhow!("hosts 文件不存在：{HOSTS_PATH}"));
    }

    // 读取现有 hosts
    let reader = BufReader::new(File::open(HOSTS_PATH)?);
    let mut lines = Vec::<String>::new();
    let mut in_section = false;
    let mut marker_found = false;

    for l in reader.lines() {
        let l = l?;
        if l.trim() == HOSTS_MARKER_START {
            marker_found = true;
            in_section = true;
            lines.push(l.clone());
            continue;
        }
        if l.trim() == HOSTS_MARKER_END {
            in_section = false;
            lines.push(l.clone());
            continue;
        }
        if !in_section {
            lines.push(l);
        }
    }

    if !marker_found {
        lines.push(String::new());
        lines.push(HOSTS_MARKER_START.into());
        lines.push(HOSTS_MARKER_END.into());
    }

    // 插入条目的位置
    let pos = lines
        .iter()
        .position(|l| l.trim() == HOSTS_MARKER_START)
        .map(|i| i + 1)
        .unwrap();

    // 构造条目
    let mut entries: Vec<_> = best.iter().map(|(d, ip)| format!("{ip} {d}")).collect();
    entries.sort();

    for (i, e) in entries.iter().enumerate() {
        lines.insert(pos + i, e.clone());
    }

    // 写临时文件
    let tmp = "/tmp/hosts.new";
    {
        let mut f = File::create(tmp)?;
        for l in &lines {
            writeln!(f, "{l}")?;
        }
    }

    // sudo cp
    let status = Command::new("sudo")
        .args(["cp", tmp, HOSTS_PATH])
        .status()?;
    if !status.success() {
        return Err(anyhow!("sudo cp 失败"));
    }
    fs::remove_file(tmp).ok();

    println!("hosts 更新成功");
    Ok(())
}