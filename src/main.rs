use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::fs;

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
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SpeedTestTarget {
    exchange: String,
    endpoint: String,
}

// New struct for config file structure
#[derive(Deserialize)]
struct Config {
    targets: Vec<SpeedTestTarget>,
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
const CONFIG_PATH: &str = "targets.toml"; // Path to the configuration file

// Function to load targets from TOML config file
fn load_targets_from_config(path: &str) -> Result<Vec<SpeedTestTarget>> {
    let content = fs::read_to_string(path)
        .context(format!("读取配置文件 {} 失败. 请确保文件存在且格式正确.", path))?;
    
    if content.trim().is_empty() {
        println!("配置文件 {} 为空.", path);
        return Ok(Vec::new());
    }
    
    let config: Config = toml::from_str(&content)
        .context(format!("解析配置文件 {} 失败. 请检查TOML格式.", path))?;
    
    Ok(config.targets)
}

#[tokio::main]
async fn main() -> Result<()> {
    println!("TCP Speed Tester 启动中...");
    println!("结果将保存至: {}", RESULTS_PATH);
    println!("测试将每 {} 分钟运行一次", TEST_INTERVAL_MINS);
    println!("结果将保留 {} 小时", RETENTION_HOURS);
    println!("将从 {} 加载测速目标", CONFIG_PATH);
    
    // 从配置文件加载测试目标
    let targets = match load_targets_from_config(CONFIG_PATH) {
        Ok(loaded_targets) => {
            if loaded_targets.is_empty() {
                println!("配置文件 {} 中未定义任何测速目标，或文件为空. 程序将退出.", CONFIG_PATH);
                return Ok(());
            }
            println!("从 {} 成功加载 {} 个测速目标.", CONFIG_PATH, loaded_targets.len());
            loaded_targets
        }
        Err(e) => {
            eprintln!("加载配置文件 {} 失败: {}", CONFIG_PATH, e);
            println!("请创建 {} 文件并按要求配置测速目标，或者检查文件格式是否正确。", CONFIG_PATH);
            return Err(e);
        }
    };
    
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
            
            // 清理旧结果(超过指定小时)
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
            
            let mut total_time_ms = 0.0;
            let mut successful_attempts = 0;
            
            for attempt in 1..=TEST_ATTEMPTS {
                let socket_addr = SocketAddr::new(*ip, port);
                let start = Instant::now();
                
                match tokio::time::timeout(timeout, TcpStream::connect(socket_addr)).await {
                    Ok(Ok(_)) => {
                        let elapsed = start.elapsed();
                        let elapsed_ms = elapsed.as_secs_f64() * 1000.0;
                        total_time_ms += elapsed_ms;
                        successful_attempts += 1;
                        println!("    尝试 {}: 连接成功，耗时 {:.2}ms", 
                                 attempt, elapsed_ms);
                    }
                    Ok(Err(e)) => {
                        println!("    尝试 {}: 连接失败 - {}", 
                                 attempt, e);
                    }
                    Err(_) => {
                        println!("    尝试 {}: 连接超时，超过 {}s", 
                                 attempt, timeout.as_secs());
                    }
                }
                
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
    
    let file = File::open(RESULTS_PATH).context("打开结果文件失败")?;
    let mut rdr = csv::ReaderBuilder::new().from_reader(BufReader::new(file));
    
    let mut records: Vec<SpeedTestRecord> = Vec::new();
    for result in rdr.deserialize() {
        let record: SpeedTestRecord = result.context("反序列化记录失败")?;
        records.push(record);
    }
    
    let cutoff = Utc::now() - chrono::Duration::hours(RETENTION_HOURS);
    let old_count = records.len();
    records.retain(|r| r.timestamp >= cutoff);
    let new_count = records.len();
    
    if old_count == new_count {
        println!("没有旧记录需要清理");
        return Ok(());
    }
    
    let mut wtr = csv::Writer::from_path(RESULTS_PATH).context("创建结果文件用于写入失败")?;
    
    for record in &records {
        wtr.serialize(record).context("写入记录失败")?;
    }
    
    wtr.flush().context("刷新写入器失败")?;
    
    println!("已清理旧记录: 删除了 {} 条记录 (超过 {} 小时)", 
             old_count - new_count, RETENTION_HOURS);
    Ok(())
}

// =================================================================
// ==================== 新增和修改部分开始 ====================
// =================================================================

/// [新增] 检查指定IP和端口的连通性
async fn verify_connectivity(ip_str: &str, port: u16) -> bool {
    let ip: IpAddr = match ip_str.parse() {
        Ok(ip) => ip,
        Err(_) => {
            println!("  -> 无效的IP地址格式: {}", ip_str);
            return false;
        }
    };
    let socket_addr = SocketAddr::new(ip, port);
    let timeout = Duration::from_secs(CONNECTION_TIMEOUT_SECS);

    match tokio::time::timeout(timeout, TcpStream::connect(socket_addr)).await {
        Ok(Ok(_)) => true, // 连接成功
        _ => false, // 连接超时或失败
    }
}

/// [新增] 从CSV历史记录中删除所有关于某个无效IP的条目
async fn remove_stale_records_from_csv(domain_to_remove: &str, ip_to_remove: &str) -> Result<()> {
    if !Path::new(RESULTS_PATH).exists() {
        return Ok(());
    }

    // 读取所有记录
    let file = File::open(RESULTS_PATH).context("打开结果文件以进行清理失败")?;
    let mut rdr = csv::Reader::from_reader(BufReader::new(file));
    let records: Vec<SpeedTestRecord> = rdr.deserialize().collect::<Result<_, _>>()
        .context("反序列化记录以进行清理失败")?;
    
    let initial_count = records.len();

    // 过滤掉所有匹配无效domain和ip的记录
    let valid_records: Vec<SpeedTestRecord> = records
        .into_iter()
        .filter(|rec| !(rec.domain == domain_to_remove && rec.ip == ip_to_remove))
        .collect();

    let final_count = valid_records.len();

    if initial_count == final_count {
        return Ok(()); // 没有记录被删除
    }

    // 将过滤后的记录写回临时文件
    let temp_path_str = format!("{}.tmp", RESULTS_PATH);
    let temp_path = Path::new(&temp_path_str);
    {
        let mut wtr = csv::Writer::from_path(temp_path).context("创建临时结果文件失败")?;
        for record in &valid_records {
            wtr.serialize(record).context("向临时文件写入记录失败")?;
        }
        wtr.flush().context("刷新临时文件写入器失败")?;
    }

    // 用临时文件覆盖原文件
    fs::rename(temp_path, RESULTS_PATH).context("用临时文件替换原始结果文件失败")?;

    println!("  -> 已从 {} 中删除 {} 条关于失效IP {} (域名: {}) 的陈旧记录", 
             RESULTS_PATH, initial_count - final_count, ip_to_remove, domain_to_remove);

    Ok(())
}

/// [修改] 更新了 `update_hosts_file` 函数的逻辑
async fn update_hosts_file() -> Result<()> {
    if !Path::new(RESULTS_PATH).exists() {
        println!("还没有速度测试结果，跳过hosts文件更新");
        return Ok(());
    }
    
    let file = File::open(RESULTS_PATH).context("打开结果文件失败")?;
    let mut rdr = csv::ReaderBuilder::new().from_reader(BufReader::new(file));
    
    let records: Vec<SpeedTestRecord> = rdr.deserialize().collect::<Result<_,_>>()?;
    
    if records.is_empty() {
        println!("速度测试记录为空");
        return Ok(());
    }
    
    println!("从 {} 条记录中计算并验证最佳IP", records.len());
    
    // 改进的权重计算: 先分组，再计算平均值
    let mut ip_data: HashMap<(String, String), (Vec<f64>, Vec<f64>)> = HashMap::new(); // (domain, ip) -> (times, weights)
    
    for record in &records {
        let now = Utc::now();
        let hours_old = (now - record.timestamp).num_seconds() as f64 / 3600.0;
        let recency_weight = if hours_old <= 1.0 { 1.0 } else if hours_old <= 5.0 { 0.8 } else { 0.5 };
        let speed_weight = 1000.0 / (record.connection_time_ms + 10.0); // 加上小常数防止除以零
        let weight = recency_weight * speed_weight;
        
        let entry = ip_data.entry((record.domain.clone(), record.ip.clone())).or_default();
        entry.0.push(record.connection_time_ms);
        entry.1.push(weight);
    }
    
    let mut domain_ips: HashMap<String, Vec<WeightedIpResult>> = HashMap::new();
    for ((domain, ip), (times, weights)) in ip_data {
        let avg_speed = times.iter().sum::<f64>() / times.len() as f64;
        let avg_weight = weights.iter().sum::<f64>() / weights.len() as f64;
        
        domain_ips.entry(domain).or_default().push(WeightedIpResult {
            ip,
            average_speed: avg_speed,
            weight: avg_weight,
        });
    }

    let mut best_ips: HashMap<String, String> = HashMap::new();
    
    for (domain, ips) in &mut domain_ips {
        // 按权重降序排序，选出最优的IP进行尝试
        ips.sort_by(|a, b| b.weight.partial_cmp(&a.weight).unwrap_or(std::cmp::Ordering::Equal));
        
        let port = match records.iter().find(|r| r.domain == *domain) {
            Some(r) => r.port,
            None => {
                println!("警告: 未能在记录中找到域名 {} 的端口信息，跳过此域名。", domain);
                continue;
            }
        };

        let mut valid_ip_found = false;
        for candidate in ips {
            println!("正在验证 {} 的候选IP: {} (端口: {})", domain, candidate.ip, port);

            if verify_connectivity(&candidate.ip, port).await {
                println!("  -> IP {} 验证通过. 平均速度: {:.2}ms, 权重: {:.2}", 
                         candidate.ip, candidate.average_speed, candidate.weight);
                best_ips.insert(domain.clone(), candidate.ip.clone());
                valid_ip_found = true;
                break; // 找到可用IP，处理下一个域名
            } else {
                println!("  -> IP {} 验证失败. 正在从历史记录中移除...", candidate.ip);
                remove_stale_records_from_csv(domain, &candidate.ip).await?;
            }
        }

        if !valid_ip_found {
            println!("警告: 在对域名 {} 的所有候选IP进行验证后，未找到可用IP。", domain);
        }
    }
    
    // 使用经过验证的IP更新hosts文件
    update_linux_hosts_file(&best_ips).context("更新hosts文件失败")
}

// ===============================================================
// ==================== 新增和修改部分结束 ====================
// ===============================================================

fn update_linux_hosts_file(best_ips: &HashMap<String, String>) -> Result<()> {
    if best_ips.is_empty() {
        println!("没有最佳IP需要更新到hosts文件");
        return Ok(());
    }
    
    if !Path::new(HOSTS_PATH).exists() {
        return Err(anyhow::anyhow!("hosts文件 {} 不存在", HOSTS_PATH));
    }
    
    println!("读取当前hosts文件: {}", HOSTS_PATH);
    
    let file = File::open(HOSTS_PATH).context("打开hosts文件失败")?;
    
    let mut new_lines = Vec::new();
    let mut in_custom_section = false;
    let mut section_content_removed = false;

    // 读取并过滤掉我们管理的部分
    for line in BufReader::new(file).lines() {
        let line = line.context("读取hosts文件行失败")?;
        
        if line.trim() == HOSTS_MARKER_START {
            in_custom_section = true;
        } else if line.trim() == HOSTS_MARKER_END {
            in_custom_section = false;
            section_content_removed = true;
        } else if !in_custom_section {
            new_lines.push(line);
        }
    }
    
    // 如果没有找到我们的标记，则在文件末尾添加
    if !section_content_removed {
        new_lines.push(String::new()); // 空行
        new_lines.push(HOSTS_MARKER_START.to_string());
        new_lines.push(HOSTS_MARKER_END.to_string());
    }

    // 准备要插入的新条目
    let mut entries_to_insert = Vec::new();
    entries_to_insert.push(HOSTS_MARKER_START.to_string());
    let mut sorted_best_ips: Vec<_> = best_ips.iter().collect();
    sorted_best_ips.sort_by_key(|(domain, _)| *domain);

    println!("以下条目将被写入hosts文件:");
    for (domain, ip) in sorted_best_ips {
        let entry = format!("{} {}", ip, domain);
        println!("  {}", entry);
        entries_to_insert.push(entry);
    }
    entries_to_insert.push(HOSTS_MARKER_END.to_string());

    // 将新条目添加到内容中
    new_lines.extend(entries_to_insert);

    // 创建临时文件用于写入
    let temp_path = "/tmp/hosts.new";
    let mut temp_file = File::create(temp_path).context("创建临时hosts文件失败")?;
    
    writeln!(temp_file, "{}", new_lines.join("\n")).context("写入临时hosts文件失败")?;
    
    // 使用sudo替换hosts文件
    println!("使用sudo更新hosts文件...");
    let status = Command::new("sudo")
        .args(["cp", temp_path, HOSTS_PATH])
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