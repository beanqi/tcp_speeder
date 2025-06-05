use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize};
use tokio::net::TcpStream;
use tokio::time::sleep;
use trust_dns_resolver::config::{ResolverConfig, ResolverOpts};
use trust_dns_resolver::TokioAsyncResolver;

// 记录测速结果的结构体
#[derive(Debug, Serialize, Deserialize, Clone)]
struct SpeedTestRecord {
    exchange: String,
    endpoint: String,
    domain: String,
    port: u16,
    ip: String,
    timestamp: DateTime<Utc>,
    connection_time_ms: f64,
}

// 测速目标结构体
#[derive(Debug, Clone)]
struct SpeedTestTarget {
    exchange: String,
    endpoint: String,
}

// 加权IP结果结构体
#[derive(Debug)]
struct WeightedIpResult {
    ip: String,
    average_speed: f64,
    weight: f64,
}

// 常量定义
const RESULTS_PATH: &str = "tcp_speed_results.csv";
const HOSTS_PATH: &str = "/etc/hosts";
const HOSTS_MARKER_START: &str = "# TCP Speed Test";
const HOSTS_MARKER_END: &str = "# End TCP Speed Test";
const TEST_ATTEMPTS: usize = 5; // 每个IP测速次数
const CONNECTION_TIMEOUT_SECS: u64 = 5; // 连接超时时间，5s
const TEST_INTERVAL_MINS: u64 = 5; // 每5分钟执行一次
const RETENTION_HOURS: i64 = 1; // 1小时后清理旧记录

#[tokio::main]
async fn main() -> Result<()> {
    println!("TCP Speed Tester 启动中...");
    println!("结果将保存至: {}", RESULTS_PATH);
    println!("测试将每 {} 分钟运行一次", TEST_INTERVAL_MINS);
    println!("结果将保留 {} 小时", RETENTION_HOURS);
    
    // 定义测试目标
    let targets = vec![
        SpeedTestTarget { exchange: "Binance".to_string(), endpoint: "api.binance.com:443".to_string() },
        SpeedTestTarget { exchange: "Binance".to_string(), endpoint: "stream.binance.com:9443".to_string() },
        SpeedTestTarget { exchange: "Huobi".to_string(), endpoint: "api-aws.huobi.pro:443".to_string() },
        SpeedTestTarget { exchange: "Huobi".to_string(), endpoint: "api-aws.huobi.pro:443".to_string() },
        SpeedTestTarget { exchange: "OKEX".to_string(), endpoint: "www.okx.com:443".to_string() },
        SpeedTestTarget { exchange: "OKEX".to_string(), endpoint: "ws.okx.com:8443".to_string() },
        SpeedTestTarget { exchange: "Coinbase".to_string(), endpoint: "api.exchange.coinbase.com:443".to_string() },
        SpeedTestTarget { exchange: "Coinbase".to_string(), endpoint: "ws-feed.exchange.coinbase.com:443".to_string() },
        SpeedTestTarget { exchange: "Kraken".to_string(), endpoint: "api.kraken.com:443".to_string() },
        SpeedTestTarget { exchange: "Kraken".to_string(), endpoint: "ws.kraken.com:443".to_string() },
        SpeedTestTarget { exchange: "Gate".to_string(), endpoint: "api.gateio.ws:443".to_string() },
        SpeedTestTarget { exchange: "Gate".to_string(), endpoint: "api.gateio.ws:443".to_string() },
        SpeedTestTarget { exchange: "KuCoin".to_string(), endpoint: "openapi-v2.kucoin.com:443".to_string() },
        SpeedTestTarget { exchange: "KuCoin".to_string(), endpoint: "ws-api-spot.kucoin.com:443".to_string() },
        SpeedTestTarget { exchange: "BitGet".to_string(), endpoint: "api.bitget.com:443".to_string() },
        SpeedTestTarget { exchange: "BitGet".to_string(), endpoint: "ws.bitget.com:443".to_string() },
        SpeedTestTarget { exchange: "MXC".to_string(), endpoint: "api.mexc.co:443".to_string() },
        SpeedTestTarget { exchange: "MXC".to_string(), endpoint: "wbs.mexc.co:443".to_string() },
        SpeedTestTarget { exchange: "Bybit".to_string(), endpoint: "api.bybit.com:443".to_string() },
        SpeedTestTarget { exchange: "Bybit".to_string(), endpoint: "stream.bybit.com:443".to_string() },
        SpeedTestTarget { exchange: "CoinEx".to_string(), endpoint: "api.coinex.com:443".to_string() },
        SpeedTestTarget { exchange: "CoinEx".to_string(), endpoint: "socket.coinex.com:443".to_string() },
        SpeedTestTarget { exchange: "Crypto".to_string(), endpoint: "api.crypto.com:443".to_string() },
        SpeedTestTarget { exchange: "Crypto".to_string(), endpoint: "stream.crypto.com:443".to_string() },
        SpeedTestTarget { exchange: "HashKey".to_string(), endpoint: "api-pro.hashkey.com:443".to_string() },
        SpeedTestTarget { exchange: "HashKey".to_string(), endpoint: "stream-pro.hashkey.com:443".to_string() },
        SpeedTestTarget { exchange: "HashKeyGlobal".to_string(), endpoint: "api-glb.hashkey.com:443".to_string() },
        SpeedTestTarget { exchange: "HashKeyGlobal".to_string(), endpoint: "stream-glb.hashkey.com:443".to_string() },
        SpeedTestTarget { exchange: "BackPack".to_string(), endpoint: "api.backpack.exchange:443".to_string() },
        SpeedTestTarget { exchange: "BackPack".to_string(), endpoint: "ws.backpack.exchange:443".to_string() },
        SpeedTestTarget { exchange: "BtcTurk".to_string(), endpoint: "api.btcturk.com:443".to_string() },
        SpeedTestTarget { exchange: "BtcTurk".to_string(), endpoint: "ws-feed-pro.btcturk.com:443".to_string() },
    ];

    // 初始化DNS解析器
    let resolver = TokioAsyncResolver::tokio(ResolverConfig::default(), ResolverOpts::default())
        .context("创建DNS解析器失败")?;
    let resolver = Arc::new(resolver);

    // 主循环 - 每5分钟运行一次
    loop {
        let start_time = Local::now();
        println!("开始测速: {}", start_time);
        
        // 运行所有目标的速度测试
        let results = match run_speed_tests(&targets, resolver.clone()).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("测速出错: {}", e);
                Vec::new()
            }
        };
        
        if !results.is_empty() {
            // 保存结果
            if let Err(e) = save_results(&results).await {
                eprintln!("保存结果失败: {}", e);
            }
            
            // 清理旧结果(超过10小时)
            if let Err(e) = cleanup_old_results().await {
                eprintln!("清理旧结果失败: {}", e);
            }
            
            // 计算最佳IP并更新hosts文件
            if let Err(e) = update_hosts_file().await {
                eprintln!("更新hosts文件失败: {}", e);
            }
        } else {
            println!("本次测试没有结果保存");
        }
        
        let end_time = Local::now();
        let elapsed = end_time.signed_duration_since(start_time);
        let elapsed_secs = elapsed.num_seconds();
        
        println!("测速完成，用时 {} 秒", elapsed_secs);
        
        // 计算睡眠时间(5分钟减去已用时间)
        let sleep_secs = (TEST_INTERVAL_MINS * 60) as i64 - elapsed_secs;
        if sleep_secs > 0 {
            println!("下次测试将在 {} 秒后进行", sleep_secs);
            sleep(Duration::from_secs(sleep_secs as u64)).await;
        } else {
            println!("测试用时超过间隔时间，立即开始下一次测试");
        }
    }
}

async fn run_speed_tests(
    targets: &[SpeedTestTarget],
    resolver: Arc<TokioAsyncResolver>,
) -> Result<Vec<SpeedTestRecord>> {
    let mut results = Vec::new();
    let timeout = Duration::from_secs(CONNECTION_TIMEOUT_SECS);

    for (index, target) in targets.iter().enumerate() {
        println!("[{}/{}] 测试 {} - {}", 
                 index + 1, targets.len(), target.exchange, target.endpoint);
        
        // 分割端点为主机名和端口
        let parts: Vec<&str> = target.endpoint.split(':').collect();
        if parts.len() != 2 {
            println!("  无效的端点格式: {}", target.endpoint);
            continue;
        }
        
        let domain = parts[0].to_string();
        let port: u16 = match parts[1].parse() {
            Ok(p) => p,
            Err(e) => {
                println!("  解析端口号失败 {}: {}", target.endpoint, e);
                continue;
            }
        };
        
        // DNS解析获取IP地址
        println!("  解析 {} 的DNS", domain);
        let lookup = match resolver.lookup_ip(&domain).await {
            Ok(l) => l,
            Err(e) => {
                println!("  DNS解析 {} 失败: {}", domain, e);
                continue;
            }
        };
        
        let ips: Vec<IpAddr> = lookup.iter().collect();
        if ips.is_empty() {
            println!("  未找到 {} 的IP地址", domain);
            continue;
        }
        
        println!("  为 {} 找到 {} 个IP地址", domain, ips.len());
        
        for (ip_index, ip) in ips.iter().enumerate() {
            println!("  测试IP [{}/{}]: {}", ip_index + 1, ips.len(), ip);
            
            // 测试此IP的TCP连接速度(5次尝试的平均值)
            let mut total_time_ms = 0.0;
            let mut successful_attempts = 0;
            
            for attempt in 1..=TEST_ATTEMPTS {
                let socket_addr = SocketAddr::new(*ip, port);
                let start = Instant::now();
                
                // 使用tokio::time::timeout限制连接时间
                match tokio::time::timeout(timeout, TcpStream::connect(socket_addr)).await {
                    Ok(result) => {
                        match result {
                            Ok(_) => {
                                let elapsed = start.elapsed();
                                let elapsed_ms = elapsed.as_secs_f64() * 1000.0;
                                total_time_ms += elapsed_ms;
                                successful_attempts += 1;
                                println!("    尝试 {}: 连接成功，耗时 {:.2}ms", 
                                         attempt, elapsed_ms);
                            }
                            Err(e) => {
                                println!("    尝试 {}: 连接失败 - {}", 
                                         attempt, e);
                            }
                        }
                    }
                    Err(_) => {
                        println!("    尝试 {}: 连接超时，超过 {}s", 
                                 attempt, timeout.as_secs());
                    }
                }
                
                // 连接尝试之间添加小延迟
                sleep(Duration::from_millis(100)).await;
            }
            
            if successful_attempts > 0 {
                let avg_time_ms = total_time_ms / successful_attempts as f64;
                
                let record = SpeedTestRecord {
                    exchange: target.exchange.clone(),
                    endpoint: target.endpoint.clone(),
                    domain: domain.clone(),
                    port,
                    ip: ip.to_string(),
                    timestamp: Utc::now(),
                    connection_time_ms: avg_time_ms,
                };
                
                println!("    结果: 平均时间: {:.2}ms ({}/{} 次成功)", 
                         avg_time_ms, successful_attempts, TEST_ATTEMPTS);
                results.push(record);
            } else {
                println!("    结果: 所有连接尝试均失败");
            }
        }
    }

    println!("测速完成，共 {} 个结果", results.len());
    Ok(results)
}

async fn save_results(results: &[SpeedTestRecord]) -> Result<()> {
    // 创建或打开CSV文件
    let file_exists = Path::new(RESULTS_PATH).exists();
    
    let file = OpenOptions::new()
        .write(true)
        .append(true)
        .create(true)
        .open(RESULTS_PATH)
        .context("打开结果文件失败")?;
    
    let mut wtr = csv::WriterBuilder::new()
        .has_headers(!file_exists)
        .from_writer(file);
    
    // 写入结果
    for record in results {
        wtr.serialize(record).context("写入记录失败")?;
    }
    
    wtr.flush().context("刷新写入器失败")?;
    
    println!("结果已保存到 {}", RESULTS_PATH);
    Ok(())
}

async fn cleanup_old_results() -> Result<()> {
    if !Path::new(RESULTS_PATH).exists() {
        return Ok(());
    }
    
    // 读取所有记录
    let file = File::open(RESULTS_PATH).context("打开结果文件失败")?;
    let reader = BufReader::new(file);
    let mut rdr = csv::ReaderBuilder::new().from_reader(reader);
    
    let mut records: Vec<SpeedTestRecord> = Vec::new();
    for result in rdr.deserialize() {
        let record: SpeedTestRecord = result.context("反序列化记录失败")?;
        records.push(record);
    }
    
    // 过滤掉超过指定小时数的记录
    let cutoff = Utc::now() - chrono::Duration::hours(RETENTION_HOURS);
    let old_count = records.len();
    records.retain(|r| r.timestamp >= cutoff);
    let new_count = records.len();
    
    if old_count == new_count {
        println!("没有旧记录需要清理");
        return Ok(());
    }
    
    // 写回过滤后的记录
    let file = File::create(RESULTS_PATH).context("创建结果文件失败")?;
    let mut wtr = csv::Writer::from_writer(file);
    
    for record in &records {
        wtr.serialize(record).context("写入记录失败")?;
    }
    
    wtr.flush().context("刷新写入器失败")?;
    
    println!("已清理旧记录: 删除了 {} 条记录 (超过 {} 小时)", 
             old_count - new_count, RETENTION_HOURS);
    Ok(())
}

async fn update_hosts_file() -> Result<()> {
    // 读取当前速度测试结果
    if !Path::new(RESULTS_PATH).exists() {
        println!("还没有速度测试结果");
        return Ok(());
    }
    
    let file = File::open(RESULTS_PATH).context("打开结果文件失败")?;
    let reader = BufReader::new(file);
    let mut rdr = csv::ReaderBuilder::new().from_reader(reader);
    
    let mut records: Vec<SpeedTestRecord> = Vec::new();
    for result in rdr.deserialize() {
        let record: SpeedTestRecord = result.context("反序列化记录失败")?;
        records.push(record);
    }
    
    if records.is_empty() {
        println!("没有发现速度测试记录");
        return Ok(());
    }
    
    println!("从 {} 条记录中计算最佳IP", records.len());
    
    // 计算每个域名的最佳IP
    let mut domain_ips: HashMap<String, Vec<WeightedIpResult>> = HashMap::new();
    
    for record in &records {
        // 基于时效性和速度计算权重
        let now = Utc::now();
        let hours_old = (now - record.timestamp).num_seconds() as f64 / 3600.0;
        let recency_weight = if hours_old <= 1.0 {
            1.0
        } else if hours_old <= 5.0 {
            0.8
        } else {
            0.5
        };
        
        // 反转连接时间(较低更好)
        let speed_weight = 1000.0 / (record.connection_time_ms + 10.0); // 添加小常数避免除以零
        
        let weight = recency_weight * speed_weight;
        
        // 存储加权IP结果
        let entry = domain_ips.entry(record.domain.clone()).or_insert_with(Vec::new);
        
        // 检查是否已有此IP
        let mut found = false;
        for ip_result in entry.iter_mut() {
            if ip_result.ip == record.ip {
                // 用新权重和平均速度更新现有条目
                ip_result.weight = (ip_result.weight + weight) / 2.0;
                ip_result.average_speed = (ip_result.average_speed + record.connection_time_ms) / 2.0;
                found = true;
                break;
            }
        }
        
        if !found {
            entry.push(WeightedIpResult {
                ip: record.ip.clone(),
                average_speed: record.connection_time_ms,
                weight,
            });
        }
    }
    
    // 找出每个域的最佳IP(最高权重)
    let mut best_ips: HashMap<String, String> = HashMap::new();
    
    for (domain, ips) in &domain_ips {
        if let Some(best) = ips.iter().max_by(|a, b| {
            a.weight.partial_cmp(&b.weight).unwrap_or(std::cmp::Ordering::Equal)
        }) {
            best_ips.insert(domain.clone(), best.ip.clone());
            println!("{} 的最佳IP: {} (平均速度: {:.2}ms, 权重: {:.2})", 
                     domain, best.ip, best.average_speed, best.weight);
        }
    }
    
    // 更新hosts文件
    update_linux_hosts_file(&best_ips).context("更新hosts文件失败")
}

fn update_linux_hosts_file(best_ips: &HashMap<String, String>) -> Result<()> {
    if best_ips.is_empty() {
        println!("没有最佳IP需要更新到hosts文件");
        return Ok(());
    }
    
    // 确保hosts文件存在
    if !Path::new(HOSTS_PATH).exists() {
        return Err(anyhow::anyhow!("hosts文件 {} 不存在", HOSTS_PATH));
    }
    
    println!("读取当前hosts文件: {}", HOSTS_PATH);
    
    // 读取当前hosts文件
    let file = File::open(HOSTS_PATH).context("打开hosts文件失败")?;
    let reader = BufReader::new(file);
    
    // 处理hosts文件
    let mut new_lines = Vec::new();
    let mut in_custom_section = false;
    let mut found_marker = false;
    
    for line in reader.lines() {
        let line = line.context("读取hosts文件行失败")?;
        
        if line.trim() == HOSTS_MARKER_START {
            in_custom_section = true;
            found_marker = true;
            new_lines.push(line);
            continue;
        }
        
        if line.trim() == HOSTS_MARKER_END {
            in_custom_section = false;
            new_lines.push(line);
            continue;
        }
        
        if !in_custom_section {
            new_lines.push(line);
        }
    }
    
    // 如果没有找到标记，在末尾添加
    if !found_marker {
        println!("向hosts文件添加TCP Speed Test标记");
        new_lines.push(String::new()); // 空行作为间隔
        new_lines.push(HOSTS_MARKER_START.to_string());
        new_lines.push(HOSTS_MARKER_END.to_string());
    }
    
    // 找到插入条目的位置
    let mut insert_pos = 0;
    for (i, line) in new_lines.iter().enumerate() {
        if line.trim() == HOSTS_MARKER_START {
            insert_pos = i + 1;
            break;
        }
    }
    
    // 插入我们的条目
    let mut entries = Vec::new();
    for (domain, ip) in best_ips {
        entries.push(format!("{} {}", ip, domain));
    }
    
    // 排序条目以便输出一致
    entries.sort();
    
    // 在正确位置插入条目
    for entry in entries {
        new_lines.insert(insert_pos, entry);
        insert_pos += 1;
    }
    
    // 创建临时文件用于写入
    let temp_path = "/tmp/hosts.new";
    let mut temp_file = File::create(temp_path).context("创建临时hosts文件失败")?;
    
    // 写入新内容
    for line in new_lines {
        writeln!(temp_file, "{}", line).context("写入临时hosts文件失败")?;
    }
    
    // 使用sudo替换hosts文件
    println!("使用sudo更新hosts文件...");
    let status = Command::new("sudo")
        .args(&["cp", temp_path, HOSTS_PATH])
        .status()
        .context("执行sudo cp命令失败")?;
    
    if !status.success() {
        return Err(anyhow::anyhow!("使用sudo更新hosts文件失败"));
    }
    
    // 清理临时文件
    if let Err(e) = std::fs::remove_file(temp_path) {
        println!("警告: 删除临时文件 {} 失败: {}", temp_path, e);
    }
    
    println!("hosts文件更新成功");
    Ok(())
}