use irae::{Kind, Result, Rollout};
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq)]
enum Workload {
    /// A deployment with a namespace (if different from context)
    Deployment(String, Option<String>),
    /// A statefulset with a namespace (if different from context)
    StatefulSet(String, Option<String>),
    // A daemonset with a namespace (if different from context)
    DaemonSet(String, Option<String>),
    // TODO: ks,
}

impl FromStr for Workload {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let lower = value.to_ascii_lowercase();
        let split = lower.splitn(3, '/').collect::<Vec<_>>();
        let (ns, kind, name) = match split.as_slice() {
            [ns, kind, name] => (Some(ns.to_string()), kind.to_string(), name.to_string()),
            [kind, name] => (None, kind.to_string(), name.to_string()),
            _ => anyhow::bail!("unknown workload split; syntax: kind/name or ns/kind/name"),
        };
        match kind.as_ref() {
            "deploy" | "deployment" => Ok(Self::Deployment(name, ns)),
            "sts" | "statefulset" => Ok(Self::StatefulSet(name, ns)),
            "ds" | "daemonset" => Ok(Self::DaemonSet(name, ns)),
            _ => anyhow::bail!("unknown kind: {kind}. we support deploy/sts/ds"),
        }
    }
}

#[derive(clap::Parser, Debug)]
#[clap(arg_required_else_help = true)]
struct Irt {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Subcommand, Debug)]
/// Commands for terminal irae
pub enum Command {
    /// Track workload(s)
    Track(TrackArgs),
    // TODO: doctor / diagnose / ..
}

#[derive(clap::Parser, Debug)]
pub struct TrackArgs {
    /// Comma-separated list of workloads to track
    ///
    /// Example: --workloads="monitoring/deploy/grafana,monitoring/sts/prometheus"
    #[clap(long, short = 'w', use_value_delimiter = true, value_parser = Workload::from_str)]
    workloads: Vec<Workload>,

    /// The namespace to use for all workloads
    ///
    /// This overrides for all workloads not already set.
    #[clap(short = 'n', long)]
    namespace: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Ignore SIGPIPE errors to avoid having to use let _ = write! everywhere
    // See https://github.com/rust-lang/rust/issues/46016
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let cli = <Irt as clap::Parser>::parse();
    match cli.command {
        Command::Track(args) => handle_track(args).await?,
    }
    Ok(())
}

async fn handle_track(args: TrackArgs) -> Result<()> {
    let client = kube::Client::try_default().await.unwrap();
    for wl in args.workloads {
        let (kind, name, ns) = match wl {
            Workload::Deployment(name, ns) => (Kind::Deployment, name, ns),
            Workload::StatefulSet(name, ns) => (Kind::StatefulSet, name, ns),
            Workload::DaemonSet(name, ns) => (Kind::DaemonSet, name, ns),
        };
        let r = Rollout {
            name,
            namespace: ns.or_else(|| args.namespace.clone()),
            workload: kind,
            client: client.clone(),
        };
        let (outcome, state) = irae::term::workload_rollout(&r).await?;
        if !outcome {
            r.debug(&state).await?;
        }
    }
    Ok(())
}
