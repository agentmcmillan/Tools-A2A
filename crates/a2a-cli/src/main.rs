/// a2a — CLI operator tool for the a2a-gateway.
///
/// Commands:
///   health
///   agents list
///   agents call <name> <json>
///   identity soul <agent>
///   identity todos <agent>
///   projects list
///   projects add <name> --repo <url>
///   projects show <name>
///   projects log <name>
///   peers list
///   peers ping <endpoint>
///   groups create <name>
///   groups invite <group-name>
///   groups join <endpoint> <token>
///   lxc list
///   lxc spawn <agent-name>
///   lxc stop <vmid>

use a2a_proto::local::{
    local_gateway_client::LocalGatewayClient,
    AgentRef, CallRequest, Empty, TodoFilter,
};
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde_json::json;
use tonic::transport::Channel;
use uuid::Uuid;

#[derive(Parser)]
#[command(name = "a2a", about = "Agent-to-Agent gateway operator CLI")]
struct Cli {
    #[arg(long, env = "A2A_GATEWAY", default_value = "http://127.0.0.1:7240")]
    gateway: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Show gateway health
    Health,

    /// Agent management
    Agents {
        #[command(subcommand)]
        action: AgentsAction,
    },

    /// Agent identity (soul, memory, todos)
    Identity {
        #[command(subcommand)]
        action: IdentityAction,
    },

    /// Project management
    Projects {
        #[command(subcommand)]
        action: ProjectsAction,
    },

    /// Peer gateway management (uses web API :7243)
    Peers {
        #[command(subcommand)]
        action: PeersAction,
    },

    /// Group management
    Groups {
        #[command(subcommand)]
        action: GroupsAction,
    },

    /// LXC container management (requires Proxmox)
    Lxc {
        #[command(subcommand)]
        action: LxcAction,
    },
}

#[derive(Subcommand)]
enum AgentsAction {
    List,
    Call {
        name:    String,
        payload: String,
        #[arg(long, default_value = "")]
        caller: String,
    },
    /// Generate a PRD from the orchestrator's current state
    Prd,
}

#[derive(Subcommand)]
enum IdentityAction {
    Soul { agent: String },
    Todos {
        agent: String,
        #[arg(long)]
        all: bool,
    },
}

#[derive(Subcommand)]
enum ProjectsAction {
    /// List all projects
    List,
    /// Show a specific project's detail
    Show {
        name: String,
    },
    /// Add a new local project
    Add {
        name: String,
        #[arg(long)]
        repo: String,
        #[arg(long, default_value = "")]
        description: String,
    },
    /// Show the task log for a project
    Log {
        name: String,
        #[arg(long, default_value = "0")]
        after: i64,
    },
    /// Write a task log entry
    Focus {
        name:    String,
        content: String,
        #[arg(long, default_value = "focus")]
        entry_type: String,
        #[arg(long, default_value = "")]
        agent: String,
    },
}

#[derive(Subcommand)]
enum PeersAction {
    /// List configured peers
    List,
    /// Add a peer gateway (appends to gateway.toml)
    Add {
        /// Peer gateway name (e.g. "hub", "site-b")
        #[arg(long)]
        name: String,
        /// Peer gateway gRPC endpoint (e.g. http://192.168.1.67:7241)
        #[arg(long)]
        endpoint: String,
        /// Optional TLS cert path for mTLS
        #[arg(long)]
        cert: Option<String>,
    },
    /// Ping a peer gateway
    Ping {
        endpoint: String,
    },
    /// List projects visible on a peer
    ListProjects {
        endpoint: String,
    },
}

#[derive(Subcommand)]
enum GroupsAction {
    /// List groups on this gateway
    List,
    /// Create a new group
    Create {
        name: String,
        #[arg(long, default_value = "")]
        description: String,
    },
    /// Generate an invite token for a group
    Invite {
        group_name: String,
    },
    /// Join a group on a remote gateway using an invite token
    Join {
        endpoint:     String,
        invite_token: String,
    },
}

#[derive(Subcommand)]
enum LxcAction {
    /// List all LXC containers on the Proxmox node
    List,
    /// Clone the agent template and start a new container
    Spawn {
        /// Agent name (container will be named a2a-<name>)
        name: String,
    },
    /// Gracefully shut down a running container
    Stop {
        /// Proxmox VM ID
        vmid: u32,
    },
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match &cli.command {
        // Commands that use the web API directly
        Commands::Projects { action } => {
            return cmd_projects(&cli.gateway, action).await;
        }
        Commands::Groups { action } => {
            return cmd_groups(&cli.gateway, action).await;
        }
        Commands::Peers { action } => {
            return cmd_peers(&cli.gateway, action).await;
        }
        Commands::Lxc { action } => {
            return cmd_lxc(&cli.gateway, action).await;
        }
        _ => {}
    }

    // Commands that use the gRPC local gateway
    let channel = Channel::from_shared(cli.gateway.clone())
        .context("invalid gateway URL")?
        .connect()
        .await
        .with_context(|| format!("connecting to gateway at {}", cli.gateway))?;
    let mut client = LocalGatewayClient::new(channel);

    match cli.command {
        Commands::Health => cmd_health(&mut client).await?,
        Commands::Agents { action } => match action {
            AgentsAction::List => cmd_agents_list(&mut client).await?,
            AgentsAction::Call { name, payload, caller } =>
                cmd_agents_call(&mut client, &name, &payload, &caller).await?,
            AgentsAction::Prd =>
                cmd_agents_prd(&mut client).await?,
        },
        Commands::Identity { action } => match action {
            IdentityAction::Soul { agent }       => cmd_identity_soul(&mut client, &agent).await?,
            IdentityAction::Todos { agent, all } => cmd_identity_todos(&mut client, &agent, all).await?,
        },
        _ => unreachable!(),
    }

    Ok(())
}

// ── gRPC handlers ─────────────────────────────────────────────────────────────

async fn cmd_health(client: &mut LocalGatewayClient<Channel>) -> Result<()> {
    let h = client.health(Empty {}).await?.into_inner();
    println!("ok:             {}", h.ok);
    println!("agents:         {}", h.agent_count);
    println!("peers:          {}", h.peer_count);
    println!("nats:           {}", h.nats_ok);
    println!("reconciler_run: {}", h.reconciler_last_run);
    Ok(())
}

async fn cmd_agents_list(client: &mut LocalGatewayClient<Channel>) -> Result<()> {
    let list = client.list_agents(Empty {}).await?.into_inner();
    if list.agents.is_empty() {
        println!("(no agents registered)");
        return Ok(());
    }
    println!("{:<20} {:<10} {:<40} {}", "NAME", "VERSION", "ENDPOINT", "CAPABILITIES");
    for a in &list.agents {
        println!("{:<20} {:<10} {:<40} {}", a.name, a.version, a.endpoint, a.capabilities.join(", "));
    }
    Ok(())
}

async fn cmd_agents_call(
    client:  &mut LocalGatewayClient<Channel>,
    name:    &str,
    payload: &str,
    caller:  &str,
) -> Result<()> {
    let payload_bytes = serde_json::from_str::<serde_json::Value>(payload)
        .context("payload must be valid JSON")?
        .to_string()
        .into_bytes();

    let resp = client.call(CallRequest {
        target_agent: name.to_owned(),
        method:       "invoke".to_owned(),
        payload:      payload_bytes,
        trace_id:     Uuid::new_v4().to_string(),
        caller:       caller.to_owned(),
        hop_count:    0,
    }).await?.into_inner();

    if !resp.error.is_empty() {
        eprintln!("error: {}", resp.error);
        std::process::exit(1);
    }

    let result: serde_json::Value = serde_json::from_slice(&resp.result)
        .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&resp.result).into_owned()));
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

async fn cmd_agents_prd(client: &mut LocalGatewayClient<Channel>) -> Result<()> {
    let resp = client.call(CallRequest {
        target_agent: "orchestrator".to_owned(),
        method:       "generate_prd".to_owned(),
        payload:      b"{}".to_vec(),
        trace_id:     Uuid::new_v4().to_string(),
        caller:       "cli".to_owned(),
        hop_count:    0,
    }).await?.into_inner();

    if !resp.error.is_empty() {
        eprintln!("error: {}", resp.error);
        std::process::exit(1);
    }

    let result: serde_json::Value = serde_json::from_slice(&resp.result)
        .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&resp.result).into_owned()));

    // If the result contains a "prd" field, print just the PRD content
    if let Some(prd) = result.get("prd").and_then(|v| v.as_str()) {
        println!("{prd}");
    } else {
        println!("{}", serde_json::to_string_pretty(&result)?);
    }
    Ok(())
}

async fn cmd_identity_soul(client: &mut LocalGatewayClient<Channel>, agent: &str) -> Result<()> {
    let soul = client.get_soul(AgentRef { name: agent.to_owned() }).await?.into_inner();
    println!("{}", soul.content_toml);
    Ok(())
}

async fn cmd_identity_todos(
    client:       &mut LocalGatewayClient<Channel>,
    agent:        &str,
    include_done: bool,
) -> Result<()> {
    let todos = client.list_todos(TodoFilter {
        agent_name:   agent.to_owned(),
        include_done,
    }).await?.into_inner().todos;

    if todos.is_empty() {
        println!("(no todos)");
        return Ok(());
    }
    println!("{:>6}  {:<12}  {}", "ID", "STATUS", "TASK");
    for t in &todos {
        println!("{:>6}  {:<12}  {}", t.id, t.status, t.task);
        if !t.notes.is_empty() {
            println!("         notes: {}", t.notes);
        }
    }
    Ok(())
}

// ── Web API handlers (projects / groups / peers) ──────────────────────────────

/// Derive the web UI base URL from the gRPC gateway URL.
/// gRPC is :7240, web UI is :7243.
fn web_base(gateway: &str) -> String {
    // Replace the port with 7243, or append if no port found
    if let Some(idx) = gateway.rfind(':') {
        let host = &gateway[..idx];
        format!("{host}:7243")
    } else {
        format!("{gateway}:7243")
    }
}

async fn cmd_projects(gateway: &str, action: &ProjectsAction) -> Result<()> {
    let base = web_base(gateway);
    let client = reqwest::Client::new();

    match action {
        ProjectsAction::List => {
            let resp = client.get(format!("{base}/dashboard"))
                .send().await.context("web request")?;
            // We just print the status; for real inspection use the browser
            println!("dashboard: {base}/dashboard  (HTTP {})", resp.status());
            println!("tip: open {} in your browser for the full UI", base);
        }

        ProjectsAction::Show { name } => {
            println!("project detail: {base}/projects  (search for '{name}')");
            println!("tip: use the web UI at {base}/dashboard");
        }

        ProjectsAction::Add { name, repo, description } => {
            // Projects are created via the web UI or future RPC extension.
            // For now, print the curl equivalent.
            println!("To create a project, use the web UI or add it to gateway.toml.");
            println!("Project '{}' / repo '{}' / desc '{}'", name, repo, description);
        }

        ProjectsAction::Log { name, after } => {
            // Fetch task log from web API
            let resp = client
                .get(format!("{base}/projects/{name}/tasks?after={after}"))
                .send().await.context("fetch task log")?;
            if resp.status().is_success() {
                println!("{}", resp.text().await?);
            } else {
                eprintln!("error: {} — try `a2a projects list` first to find the project ID", resp.status());
            }
        }

        ProjectsAction::Focus { name, content, entry_type, agent } => {
            // Write a task log entry
            let form = [
                ("entry_type", entry_type.as_str()),
                ("content",    content.as_str()),
                ("agent_name", agent.as_str()),
            ];
            let resp = client
                .post(format!("{base}/projects/{name}/tasks"))
                .form(&form)
                .send().await.context("write task log")?;
            println!("status: {}", resp.status());
        }
    }
    Ok(())
}

async fn cmd_groups(gateway: &str, action: &GroupsAction) -> Result<()> {
    let base = web_base(gateway);
    match action {
        GroupsAction::List => {
            println!("Groups visible at: {base}/dashboard");
            println!("tip: full group management is in the web UI");
        }
        GroupsAction::Create { name, description } => {
            println!("Create group '{}' ({}) via web UI at {base}", name, description);
        }
        GroupsAction::Invite { group_name } => {
            println!("Generate invite for '{}' via web UI at {base}", group_name);
        }
        GroupsAction::Join { endpoint, invite_token } => {
            println!("Joining peer at '{}' with token '{}'", endpoint, &invite_token[..8]);
            println!("Use peer gRPC (port 7241) for JoinGroup RPC — or via web UI if exposed.");
        }
    }
    Ok(())
}

async fn cmd_peers(gateway: &str, action: &PeersAction) -> Result<()> {
    let base = web_base(gateway);
    match action {
        PeersAction::List => {
            println!("Peers are configured in gateway.toml [[peers]] section.");
            println!("Web UI: {base}/dashboard");
        }
        PeersAction::Add { name, endpoint, cert } => {
            // Validate endpoint URL format
            if !endpoint.starts_with("http://") && !endpoint.starts_with("https://") {
                anyhow::bail!("peer endpoint must start with http:// or https://");
            }
            // Append [[peers]] entry to the gateway config file
            let config_path = std::env::var("A2A_CONFIG").unwrap_or_else(|_| "gateway.toml".into());
            let mut entry = format!("\n[[peers]]\nname     = \"{name}\"\nendpoint = \"{endpoint}\"\n");
            if let Some(cert_path) = cert {
                entry.push_str(&format!("cert_path = \"{cert_path}\"\n"));
            }

            // Verify the peer is reachable before adding
            let client = reqwest::Client::new();
            print!("pinging {endpoint}... ");
            match client.get(format!("{endpoint}/health")).timeout(std::time::Duration::from_secs(5)).send().await {
                Ok(r) if r.status().is_success() => println!("ok (HTTP {})", r.status()),
                Ok(r) => {
                    println!("warning: HTTP {} — adding anyway", r.status());
                }
                Err(e) => {
                    println!("unreachable ({e}) — adding anyway (will sync when available)");
                }
            }

            std::fs::OpenOptions::new()
                .append(true)
                .open(&config_path)
                .and_then(|mut f| {
                    use std::io::Write;
                    f.write_all(entry.as_bytes())
                })
                .with_context(|| format!("appending to {config_path}"))?;

            println!("added peer '{name}' → {endpoint}");
            println!("restart the gateway to activate: docker compose restart gateway");
        }
        PeersAction::Ping { endpoint } => {
            let client = reqwest::Client::new();
            match client.get(format!("{endpoint}/health")).send().await {
                Ok(r)  => println!("peer {} → HTTP {}", endpoint, r.status()),
                Err(e) => eprintln!("peer {} unreachable: {e}", endpoint),
            }
        }
        PeersAction::ListProjects { endpoint } => {
            let client = reqwest::Client::new();
            match client.get(format!("{endpoint}/dashboard")).send().await {
                Ok(r)  => println!("peer dashboard {} → HTTP {}", endpoint, r.status()),
                Err(e) => eprintln!("peer {} unreachable: {e}", endpoint),
            }
        }
    }
    Ok(())
}

async fn cmd_lxc(gateway: &str, action: &LxcAction) -> Result<()> {
    let base = web_base(gateway);
    let client = reqwest::Client::new();

    match action {
        LxcAction::List => {
            let resp = client
                .get(format!("{base}/lxc"))
                .send().await.context("lxc list request")?;
            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                eprintln!("error {status}: {body}");
                std::process::exit(1);
            }
            let containers: serde_json::Value = resp.json().await.context("parsing lxc list")?;
            if let Some(arr) = containers.as_array() {
                if arr.is_empty() {
                    println!("(no containers)");
                    return Ok(());
                }
                println!("{:<8} {:<24} {:<10} {:<10} {}", "VMID", "NAME", "STATUS", "MEM(MiB)", "CPUs");
                for c in arr {
                    println!(
                        "{:<8} {:<24} {:<10} {:<10} {}",
                        c["vmid"].as_u64().unwrap_or(0),
                        c["name"].as_str().unwrap_or(""),
                        c["status"].as_str().unwrap_or(""),
                        c["maxmem"].as_u64().map(|m| m / 1024 / 1024).unwrap_or(0),
                        c["cpus"].as_f64().unwrap_or(0.0),
                    );
                }
            } else {
                println!("{}", serde_json::to_string_pretty(&containers)?);
            }
        }

        LxcAction::Spawn { name } => {
            let resp = client
                .post(format!("{base}/lxc/spawn"))
                .json(&json!({ "agent_name": name }))
                .send().await.context("lxc spawn request")?;
            let status = resp.status();
            let body: serde_json::Value = resp.json().await.unwrap_or(json!({}));
            if !status.is_success() {
                eprintln!("error {status}: {}", body);
                std::process::exit(1);
            }
            println!(
                "spawned: vmid={} name={}",
                body["vmid"].as_u64().unwrap_or(0),
                body["name"].as_str().unwrap_or(""),
            );
        }

        LxcAction::Stop { vmid } => {
            let resp = client
                .post(format!("{base}/lxc/{vmid}/stop"))
                .send().await.context("lxc stop request")?;
            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                eprintln!("error {status}: {body}");
                std::process::exit(1);
            }
            println!("shutdown signal sent to vmid {vmid}");
        }
    }
    Ok(())
}
