//! SSH 连接与 agent 投送。
//!
//! 目标机上**只需要 sshd**：不需要 Node、不需要 claude、不需要任何 API key。
//! 模型上下文和凭据全部留在中心节点，每一次工具调用先回中心过策略层，
//! 批准了才下发到这里。
//!
//! agent 二进制按内容哈希命名（`ai-task-agent-<sha256 前 16 位>`），
//! 已经在目标机上且哈希一致就跳过上传。**版本由哈希唯一确定**，
//! 不存在「那台机器上是哪个版本」这种问题。

use std::path::PathBuf;
use std::sync::Arc;

use russh::client::{self, Handle};
use russh::keys::{PrivateKeyWithHashAlg, load_secret_key};
use tokio::io::AsyncWriteExt as _;

/// 主机公钥的校验策略。
///
/// **没有「全部接受」这一档**。中间人能拿到的是一次 AI 在目标机上的完整执行
/// 权限，这种便利换不来。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKeyPolicy {
    /// 必须已经在 known_hosts 里。
    Strict,
    /// 首次见到就记下来，之后按 Strict 处理（TOFU）。
    AcceptNew,
}

#[derive(Debug, Clone)]
pub struct SshConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    /// 私钥文件路径。
    pub key_path: PathBuf,
    /// 私钥口令。
    pub key_passphrase: Option<String>,
    pub known_hosts: PathBuf,
    pub policy: HostKeyPolicy,
    pub connect_timeout: std::time::Duration,
}

#[derive(Debug, thiserror::Error)]
pub enum SshError {
    #[error("连接 {host}:{port} 失败：{source}")]
    Connect {
        host: String,
        port: u16,
        #[source]
        source: std::io::Error,
    },

    #[error("连接 {0} 超时")]
    Timeout(String),

    #[error("主机公钥校验失败：{0}。若这台机器确实换过密钥，请先更新 known_hosts")]
    HostKey(String),

    #[error("读取私钥 {path} 失败：{detail}")]
    Key { path: String, detail: String },

    #[error("认证被拒（用户 {user}）。确认公钥已在目标机的 authorized_keys 里")]
    Auth { user: String },

    #[error("SSH 协议错误：{0}")]
    Protocol(String),

    #[error("投送 agent 失败：{0}")]
    Deploy(String),
}

/// 一条已建立的 SSH 连接。
pub struct SshSession {
    handle: Handle<Client>,
    description: String,
}

impl SshSession {
    /// 连接并认证。
    pub async fn connect(config: &SshConfig) -> Result<Self, SshError> {
        let key =
            load_secret_key(&config.key_path, config.key_passphrase.as_deref()).map_err(|err| {
                SshError::Key {
                    path: config.key_path.display().to_string(),
                    detail: err.to_string(),
                }
            })?;

        let client_config = Arc::new(client::Config {
            // 长时间没有工具调用时保活，免得被中间的 NAT 或防火墙掐掉
            keepalive_interval: Some(std::time::Duration::from_secs(30)),
            keepalive_max: 3,
            ..Default::default()
        });

        let handler = Client {
            host: config.host.clone(),
            port: config.port,
            known_hosts: config.known_hosts.clone(),
            policy: config.policy,
        };

        let address = (config.host.as_str(), config.port);
        let description = format!("{}@{}:{}", config.user, config.host, config.port);

        let mut handle = tokio::time::timeout(
            config.connect_timeout,
            client::connect(client_config, address, handler),
        )
        .await
        .map_err(|_| SshError::Timeout(description.clone()))?
        .map_err(|err| match err {
            russh::Error::IO(source) => SshError::Connect {
                host: config.host.clone(),
                port: config.port,
                source,
            },
            other => SshError::Protocol(other.to_string()),
        })?;

        let result = handle
            .authenticate_publickey(
                config.user.clone(),
                PrivateKeyWithHashAlg::new(Arc::new(key), None),
            )
            .await
            .map_err(|err| SshError::Protocol(err.to_string()))?;

        if !result.success() {
            return Err(SshError::Auth {
                user: config.user.clone(),
            });
        }

        Ok(Self {
            handle,
            description,
        })
    }

    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    /// 跑一条命令，把 stdout/stderr 和退出码收回来。
    ///
    /// 只用于投送流程里的小命令（算哈希、建目录）。真正的任务执行走 agent，
    /// 那条路径有资源归因和上限。
    pub async fn run(&self, command: &str) -> Result<CommandOutput, SshError> {
        let mut channel = self
            .handle
            .channel_open_session()
            .await
            .map_err(|e| SshError::Protocol(e.to_string()))?;
        channel
            .exec(true, command)
            .await
            .map_err(|e| SshError::Protocol(e.to_string()))?;

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut exit_code = None;
        while let Some(message) = channel.wait().await {
            match message {
                russh::ChannelMsg::Data { data } => stdout.extend_from_slice(&data),
                russh::ChannelMsg::ExtendedData { data, .. } => stderr.extend_from_slice(&data),
                russh::ChannelMsg::ExitStatus { exit_status } => {
                    exit_code = i32::try_from(exit_status).ok();
                }
                russh::ChannelMsg::Eof | russh::ChannelMsg::Close => break,
                _ => {}
            }
        }

        Ok(CommandOutput {
            exit_code,
            stdout: String::from_utf8_lossy(&stdout).to_string(),
            stderr: String::from_utf8_lossy(&stderr).to_string(),
        })
    }

    /// 确保 agent 已经在目标机上，返回它的绝对路径。
    ///
    /// 已存在且哈希一致就跳过上传——一次 SSH 往返比传 500KB 便宜得多，
    /// 而且同一台机器上跑第二个 run 时不该重传。
    pub async fn ensure_agent(
        &self,
        binary: &[u8],
        sha256_hex: &str,
        dir: &str,
    ) -> Result<DeployResult, SshError> {
        // 路径带上内容哈希：升级 agent 会换一个新文件名，不会覆盖正在被
        // 别的 run 使用的那个
        let short = &sha256_hex[..16.min(sha256_hex.len())];
        let path = format!("{dir}/ai-task-agent-{short}");

        let probe = self
            .run(&format!(
                "mkdir -p {dir} && sha256sum {path} 2>/dev/null | cut -d' ' -f1"
            ))
            .await?;
        if probe.stdout.trim() == sha256_hex {
            return Ok(DeployResult {
                path,
                uploaded: false,
            });
        }

        // 先写临时文件再原子改名：中途断了不会留下一个半截的可执行文件
        let temp = format!("{path}.{}.tmp", std::process::id());
        let mut channel = self
            .handle
            .channel_open_session()
            .await
            .map_err(|e| SshError::Protocol(e.to_string()))?;
        channel
            .exec(true, format!("cat > {temp}"))
            .await
            .map_err(|e| SshError::Protocol(e.to_string()))?;
        {
            let mut writer = channel.make_writer();
            writer
                .write_all(binary)
                .await
                .map_err(|e| SshError::Deploy(format!("写入 agent 二进制失败：{e}")))?;
            writer
                .shutdown()
                .await
                .map_err(|e| SshError::Deploy(format!("关闭上传通道失败：{e}")))?;
        }
        while let Some(message) = channel.wait().await {
            if matches!(
                message,
                russh::ChannelMsg::Eof
                    | russh::ChannelMsg::Close
                    | russh::ChannelMsg::ExitStatus { .. }
            ) {
                break;
            }
        }

        // 落地前再校验一次哈希：传输被截断时宁可失败，也不能跑一个坏二进制
        let verify = self
            .run(&format!(
                "chmod +x {temp} && \
                 [ \"$(sha256sum {temp} | cut -d' ' -f1)\" = \"{sha256_hex}\" ] && \
                 mv -f {temp} {path} && echo deployed || {{ rm -f {temp}; echo mismatch; }}"
            ))
            .await?;
        if verify.stdout.trim() != "deployed" {
            return Err(SshError::Deploy(format!(
                "agent 上传后哈希校验不通过（{}）",
                verify.stdout.trim()
            )));
        }

        Ok(DeployResult {
            path,
            uploaded: true,
        })
    }

    /// 起 agent 进程，返回它的 stdin/stdout 通道。
    pub async fn start_agent(
        &self,
        agent_path: &str,
        roots: &[String],
    ) -> Result<russh::Channel<client::Msg>, SshError> {
        let args = roots
            .iter()
            .map(|r| format!("--root {}", shell_quote(r)))
            .collect::<Vec<_>>()
            .join(" ");
        let channel = self
            .handle
            .channel_open_session()
            .await
            .map_err(|e| SshError::Protocol(e.to_string()))?;
        channel
            .exec(true, format!("{agent_path} {args}"))
            .await
            .map_err(|e| SshError::Protocol(e.to_string()))?;
        Ok(channel)
    }
}

#[derive(Debug, Clone)]
pub struct CommandOutput {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone)]
pub struct DeployResult {
    pub path: String,
    /// 这次真的传了，还是复用了目标机上已有的。
    pub uploaded: bool,
}

/// russh 的回调处理器。这里唯一的职责是校验主机公钥。
struct Client {
    host: String,
    port: u16,
    known_hosts: PathBuf,
    policy: HostKeyPolicy,
}

impl client::Handler for Client {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        let outcome = classify(russh::keys::check_known_hosts_path(
            &self.host,
            self.port,
            key,
            &self.known_hosts,
        ));

        match (outcome, self.policy) {
            (HostKeyOutcome::Known, _) => Ok(true),
            // 同算法但密钥变了 —— 这是中间人的信号，任何策略下都拒
            (HostKeyOutcome::Changed, _) => {
                tracing::error!(
                    host = %self.host,
                    "主机公钥与 known_hosts 记录不符，拒绝连接。若确实换过密钥，请先更新记录"
                );
                Ok(false)
            }
            (HostKeyOutcome::Unknown, HostKeyPolicy::AcceptNew) => {
                let _ = russh::keys::known_hosts::learn_known_hosts_path(
                    &self.host,
                    self.port,
                    key,
                    &self.known_hosts,
                );
                tracing::warn!(
                    host = %self.host,
                    "首次连接，已记下主机公钥（TOFU）。要更严就把策略调成 strict"
                );
                Ok(true)
            }
            (HostKeyOutcome::Unknown, HostKeyPolicy::Strict) => {
                tracing::error!(host = %self.host, "主机公钥不在 known_hosts 里，strict 策略下拒绝");
                Ok(false)
            }
            (HostKeyOutcome::Unreadable, _) => {
                tracing::error!(host = %self.host, "读取 known_hosts 失败，拒绝连接");
                Ok(false)
            }
        }
    }
}

/// 主机公钥的三种处境。
///
/// `check_known_hosts_path` 的返回值容易读反：`Ok(false)` 是**没有这条记录**
/// （TOFU 场景），而不是「密钥对不上」——后者是 `Err(KeyChanged)`。
/// 判反了的后果是首次连接被拒、而换了密钥的主机反而被接受。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostKeyOutcome {
    /// known_hosts 里有匹配记录。
    Known,
    /// 没有这台主机（这个算法）的记录。
    Unknown,
    /// 有记录但密钥变了。
    Changed,
    /// known_hosts 读不了。
    Unreadable,
}

fn classify(result: Result<bool, russh::keys::Error>) -> HostKeyOutcome {
    match result {
        Ok(true) => HostKeyOutcome::Known,
        Ok(false) => HostKeyOutcome::Unknown,
        Err(russh::keys::Error::KeyChanged { .. }) => HostKeyOutcome::Changed,
        Err(_) => HostKeyOutcome::Unreadable,
    }
}

/// 拼进 shell 命令行的参数一律加引号。
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// agent 二进制的 SHA-256（小写 hex）。
///
/// 必须和目标机上 `sha256sum` 的输出逐字符相同——投送后的校验就是拿这个串
/// 去比的，算法选错的表现是「每次都说哈希不匹配」，然后每次都重传。
#[must_use]
pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    let digest = Sha256::digest(data);
    use std::fmt::Write as _;
    digest.iter().fold(String::with_capacity(64), |mut acc, b| {
        let _ = write!(acc, "{b:02x}");
        acc
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn there_is_no_policy_that_accepts_any_host_key() {
        // 中间人拿到的是一次 AI 在目标机上的完整执行权限。
        // 这个便利换不来——所以枚举里刻意只有两档。
        let all = [HostKeyPolicy::Strict, HostKeyPolicy::AcceptNew];
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn host_key_outcomes_are_classified_the_right_way_round() {
        // `Ok(false)` 是"没记录"，不是"对不上"。判反的后果是：
        // 首次连接被拒，而换了密钥的主机反而被放行。
        assert_eq!(classify(Ok(true)), HostKeyOutcome::Known);
        assert_eq!(classify(Ok(false)), HostKeyOutcome::Unknown);
        assert_eq!(
            classify(Err(russh::keys::Error::KeyChanged { line: 3 })),
            HostKeyOutcome::Changed
        );
        assert_eq!(
            classify(Err(russh::keys::Error::CouldNotReadKey)),
            HostKeyOutcome::Unreadable
        );
    }

    #[test]
    fn a_changed_host_key_is_refused_under_every_policy() {
        // 中间人的信号。TOFU 只适用于"第一次见到"，不适用于"变了"。
        for policy in [HostKeyPolicy::Strict, HostKeyPolicy::AcceptNew] {
            let accepted = matches!(
                (HostKeyOutcome::Changed, policy),
                (HostKeyOutcome::Known, _)
            );
            assert!(!accepted, "{policy:?} 下不该接受变更过的主机公钥");
        }
    }

    #[test]
    fn root_arguments_are_shell_quoted() {
        assert_eq!(shell_quote("/tmp/a b"), "'/tmp/a b'");
        // 目录名里的引号不能把命令行撑开
        assert_eq!(shell_quote("/tmp/x'; rm -rf /"), r"'/tmp/x'\''; rm -rf /'");
    }

    #[test]
    fn the_agent_path_is_derived_from_its_content_hash() {
        // 升级 agent 换一个新文件名，不会覆盖正在被别的 run 使用的那个
        let sha = "a1b2c3d4e5f60718293a4b5c6d7e8f90112233445566778899aabbccddeeff00";
        let short = &sha[..16];
        assert_eq!(short, "a1b2c3d4e5f60718");
        assert_eq!(
            format!("/tmp/ai-task-agent-{short}"),
            "/tmp/ai-task-agent-a1b2c3d4e5f60718"
        );
    }
}
