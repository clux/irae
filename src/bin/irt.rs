use irae::{Error, Result};
use irae::{Kind, Rollout};
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
        // TODO normal slash parsing
        //if let Some((target, derived_trait)) = value.split_once('=') {
        let lower = value.to_ascii_lowercase();
        let split = lower.splitn(3, '/').collect::<Vec<_>>();
        match split.as_slice() {
            [ns, kind, name] => Ok(Self::Deployment(name.to_string(), Some(ns.to_string()))),
            [kind, name] => Ok(Self::Deployment(name.to_string(), None)),
            //"deployment" | "deploy" => Ok(Self::Deployment),
            //"statefulset" | "sts" => Ok(Self::StatefulSet),
            _ => anyhow::bail!("unknown workload. typo? we support deploy + sts"),
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
    //#[cfg(unix)]
    //unsafe {
    //    libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    //}
    let cli = <Irt as clap::Parser>::parse();
    match cli.command {
        Command::Track(args) => handle_track(args).await?,
        //_ => anyhow::bail!("unknow subcommand"),
    }
    Ok(())
}

async fn handle_track(args: TrackArgs) -> Result<()> {
    //let mut rx = vec![];
    let client = kube::Client::try_default().await.unwrap();
    for wl in args.workloads {
        let (kind, name, ns) = match wl {
            Workload::Deployment(name, ns) => (Kind::Deployment, name, ns),
            Workload::StatefulSet(name, ns) => (Kind::StatefulSet, name, ns),
            _ => todo!(),
        };
        // TODO: fetch expectation up front
        let r = Rollout {
            name,
            namespace: ns.or_else(|| args.namespace.clone()),
            workload: kind,
            client: client.clone(),
        };
        let outcome = irae::term::workload_rollout(&r).await?;
        println!("outcome: {outcome:?}");
    }
    Ok(())
}
