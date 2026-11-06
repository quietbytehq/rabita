// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

//! `rabitactl` — command-line test harness for `rabita-helper`.
//!
//! By default it talks to the helper over the system D-Bus exactly like the
//! wizard will, so it exercises the bus policy, polkit and the signals.
//!
//! `--direct` runs the helper's library in this process instead (no D-Bus,
//! no polkit). That needs root for `join`/`leave` and exists only for
//! debugging in the lab.

use std::io::{BufRead, Write};
use std::sync::Arc;

use clap::{Parser, Subcommand};
use futures_util::StreamExt;
use rabita_helper::client::RabitaProxy;
use rabita_helper::cmd::SystemRunner;
use rabita_helper::join::JobContext;
use rabita_helper::paths::Paths;
use rabita_helper::progress::{FnSink, ProgressEvent};
use rabita_helper::types::{
    JobResult, JoinOptions, JoinRequest, LeaveRequest, LoginAccess, StepState,
};
use rabita_helper::{discover, join, leave, status, system, VERSION};
use zeroize::Zeroizing;

#[derive(Parser, Debug)]
#[command(name = "rabitactl", version = VERSION, about = "Test harness for the Rabita helper")]
struct Cli {
    /// Talk to a helper on the session bus (started with `rabita-helper --session`).
    #[arg(long, global = true)]
    session: bool,
    /// Run the helper code in this process instead of over D-Bus (lab debugging; needs root).
    #[arg(long, global = true)]
    direct: bool,
    /// Print raw JSON instead of a summary.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Show the helper's version.
    Version,
    /// What kind of computer this is.
    Detect,
    /// Find the domain controllers for a domain.
    Discover {
        /// Domain name, e.g. lab.quietbyte.test
        domain: String,
    },
    /// Connection and health report.
    Status,
    /// Join the domain.
    Join {
        /// Domain name, e.g. lab.quietbyte.test
        domain: String,
        /// Account allowed to join computers.
        #[arg(long)]
        user: String,
        /// Organizational unit for the computer object.
        #[arg(long)]
        ou: Option<String>,
        /// Computer account name (default: host name).
        #[arg(long)]
        computer_name: Option<String>,
        /// Domain user to look up afterwards to prove it worked.
        #[arg(long)]
        verify_user: Option<String>,
        /// Do not add a username box to the login screen.
        #[arg(long)]
        no_manual_login: bool,
        /// Do not create home folders on first login.
        #[arg(long)]
        no_create_home: bool,
        /// Do not cache passwords for offline login.
        #[arg(long)]
        no_offline_login: bool,
        /// Only let members of this domain group sign in (repeat for more
        /// groups). Without it, all domain users may sign in.
        #[arg(long = "allow-group", value_name = "GROUP")]
        allow_groups: Vec<String>,
        /// Read the password from standard input (first line) instead of prompting.
        #[arg(long)]
        password_stdin: bool,
    },
    /// Leave the domain and restore the original settings.
    Leave {
        /// Also delete the computer account (needs --user and a password).
        #[arg(long)]
        remove: bool,
        /// Account used to delete the computer account.
        #[arg(long)]
        user: Option<String>,
        /// Read the password from standard input (first line) instead of prompting.
        #[arg(long)]
        password_stdin: bool,
    },
    /// Re-apply options on a joined computer.
    ApplyOptions {
        /// Username box on the login screen.
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        manual_login: bool,
        /// Create home folders on first login.
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        create_home: bool,
        /// Only let members of this domain group sign in (repeat for more
        /// groups). Without it, all domain users may sign in.
        #[arg(long = "allow-group", value_name = "GROUP")]
        allow_groups: Vec<String>,
    },
    /// Print every signal the helper emits until Ctrl-C (D-Bus mode only).
    Watch,
}

fn read_password(prompt: &str, from_stdin: bool) -> Result<Zeroizing<String>, String> {
    if from_stdin {
        let mut line = String::new();
        std::io::stdin()
            .lock()
            .read_line(&mut line)
            .map_err(|e| e.to_string())?;
        Ok(Zeroizing::new(
            line.trim_end_matches(['\r', '\n']).to_owned(),
        ))
    } else {
        rpassword::prompt_password(prompt)
            .map(Zeroizing::new)
            .map_err(|e| e.to_string())
    }
}

fn print_event(ev: &ProgressEvent) {
    match ev {
        ProgressEvent::Step {
            id,
            label,
            state,
            message,
        } => {
            let mark = match state {
                StepState::Pending => "  ",
                StepState::Running => "..",
                StepState::Done => "OK",
                StepState::Failed => "!!",
                StepState::Skipped => "--",
            };
            if *state != StepState::Pending {
                let msg = if message.is_empty() {
                    String::new()
                } else {
                    format!(" — {message}")
                };
                println!("[{mark}] {label} ({id}){msg}");
            }
        }
        ProgressEvent::Log(line) => println!("      {line}"),
    }
    let _ = std::io::stdout().flush();
}

fn print_result(result: &JobResult, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(result).unwrap_or_default()
        );
    } else {
        println!();
        println!("{}", if result.ok { "SUCCESS" } else { "FAILED" });
        println!("{}", result.summary);
    }
}

fn pretty<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

fn print_json_str(s: &str) {
    match serde_json::from_str::<serde_json::Value>(s) {
        Ok(v) => println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default()),
        Err(_) => println!("{s}"),
    }
}

fn join_request(cmd: &Command) -> Option<(JoinRequest, bool)> {
    if let Command::Join {
        domain,
        user,
        ou,
        computer_name,
        verify_user,
        no_manual_login,
        no_create_home,
        no_offline_login,
        allow_groups,
        password_stdin,
    } = cmd
    {
        Some((
            JoinRequest {
                domain: domain.clone(),
                admin_user: user.clone(),
                computer_ou: ou.clone(),
                computer_name: computer_name.clone(),
                verify_user: verify_user.clone(),
                options: JoinOptions {
                    manual_login: !no_manual_login,
                    create_home: !no_create_home,
                    offline_login: !no_offline_login,
                    login_access: LoginAccess::from_groups(allow_groups),
                },
            },
            *password_stdin,
        ))
    } else {
        None
    }
}

fn run_direct(cli: &Cli) -> Result<(), String> {
    let runner = SystemRunner;
    let paths = Paths::system();
    let sink = FnSink(|ev| print_event(&ev));
    let ctx = JobContext {
        runner: &runner,
        paths: &paths,
        sink: &sink,
        network: true,
    };
    match &cli.command {
        Command::Version => println!("rabita-helper library {VERSION} (direct)"),
        Command::Detect => println!("{}", pretty(&system::detect(&runner, &paths))),
        Command::Discover { domain } => {
            println!("{}", pretty(&discover::discover(domain, &paths, true)));
        }
        Command::Status => println!("{}", pretty(&status::report(&runner, &paths, true))),
        Command::Join { .. } => {
            let (req, from_stdin) = join_request(&cli.command).expect("join");
            let password =
                read_password(&format!("Password for {}: ", req.admin_user), from_stdin)?;
            let result = join::run_join(&ctx, &req, password);
            print_result(&result, cli.json);
            if !result.ok {
                std::process::exit(1);
            }
        }
        Command::Leave {
            remove,
            user,
            password_stdin,
        } => {
            let req = LeaveRequest {
                remove_computer_account: *remove,
                admin_user: user.clone(),
            };
            let password = if *remove {
                read_password(
                    &format!("Password for {}: ", user.clone().unwrap_or_default()),
                    *password_stdin,
                )?
            } else {
                Zeroizing::new(String::new())
            };
            let result = leave::run_leave(&ctx, &req, password);
            print_result(&result, cli.json);
            if !result.ok {
                std::process::exit(1);
            }
        }
        Command::ApplyOptions {
            manual_login,
            create_home,
            allow_groups,
        } => {
            let opts = JoinOptions {
                manual_login: *manual_login,
                create_home: *create_home,
                login_access: LoginAccess::from_groups(allow_groups),
                ..JoinOptions::default()
            };
            let result = rabita_helper::service::apply_options_job(&ctx, &opts);
            print_result(&result, cli.json);
        }
        Command::Watch => return Err("watch only works over D-Bus".into()),
    }
    Ok(())
}

enum Signal {
    Progress(u32, ProgressEvent),
    Log(u32, ProgressEvent),
    Finished(u32, JobResult),
}

async fn signal_stream(
    proxy: &RabitaProxy<'_>,
) -> zbus::Result<impl futures_util::Stream<Item = Signal>> {
    let progress = proxy.receive_progress().await?.filter_map(|s| async move {
        let a = s.args().ok()?;
        Some(Signal::Progress(
            a.job,
            ProgressEvent::Step {
                id: a.step.to_owned(),
                label: a.step.to_owned(),
                state: match a.state {
                    "running" => StepState::Running,
                    "done" => StepState::Done,
                    "failed" => StepState::Failed,
                    "skipped" => StepState::Skipped,
                    _ => StepState::Pending,
                },
                message: a.message.to_owned(),
            },
        ))
    });
    let log = proxy.receive_log_line().await?.filter_map(|s| async move {
        let a = s.args().ok()?;
        Some(Signal::Log(a.job, ProgressEvent::Log(a.line.to_owned())))
    });
    let finished = proxy
        .receive_job_finished()
        .await?
        .filter_map(|s| async move {
            let a = s.args().ok()?;
            let result: JobResult = serde_json::from_str(a.result_json).ok()?;
            Some(Signal::Finished(a.job, result))
        });
    Ok(futures_util::stream::select_all(vec![
        progress.boxed(),
        log.boxed(),
        finished.boxed(),
    ]))
}

async fn wait_for_job(
    proxy: &RabitaProxy<'_>,
    stream: impl futures_util::Stream<Item = Signal>,
    job: u32,
    json: bool,
) -> JobResult {
    let _ = proxy;
    let mut stream = std::pin::pin!(stream);
    while let Some(sig) = stream.next().await {
        match sig {
            Signal::Progress(j, ev) | Signal::Log(j, ev) if j == job => print_event(&ev),
            Signal::Finished(j, result) if j == job => {
                print_result(&result, json);
                return result;
            }
            _ => {}
        }
    }
    JobResult {
        ok: false,
        summary: "Lost the connection to the helper before the job finished.".into(),
        steps: Vec::new(),
    }
}

async fn run_dbus(cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
    let conn = if cli.session {
        zbus::Connection::session().await?
    } else {
        zbus::Connection::system().await?
    };
    let proxy = RabitaProxy::new(&conn).await?;
    match &cli.command {
        Command::Version => println!("rabita-helper {}", proxy.version().await?),
        Command::Detect => print_json_str(&proxy.detect_system().await?),
        Command::Discover { domain } => print_json_str(&proxy.discover_domain(domain).await?),
        Command::Status => print_json_str(&proxy.status().await?),
        Command::Join { .. } => {
            let (req, from_stdin) = join_request(&cli.command).expect("join");
            let password =
                read_password(&format!("Password for {}: ", req.admin_user), from_stdin)?;
            let stream = signal_stream(&proxy).await?;
            let job = proxy.join(&serde_json::to_string(&req)?, &password).await?;
            drop(password);
            println!("Job {job} started.");
            let result = wait_for_job(&proxy, stream, job, cli.json).await;
            if !result.ok {
                std::process::exit(1);
            }
        }
        Command::Leave {
            remove,
            user,
            password_stdin,
        } => {
            let req = LeaveRequest {
                remove_computer_account: *remove,
                admin_user: user.clone(),
            };
            let password = if *remove {
                read_password(
                    &format!("Password for {}: ", user.clone().unwrap_or_default()),
                    *password_stdin,
                )?
            } else {
                Zeroizing::new(String::new())
            };
            let stream = signal_stream(&proxy).await?;
            let job = proxy
                .leave(&serde_json::to_string(&req)?, &password)
                .await?;
            drop(password);
            println!("Job {job} started.");
            let result = wait_for_job(&proxy, stream, job, cli.json).await;
            if !result.ok {
                std::process::exit(1);
            }
        }
        Command::ApplyOptions {
            manual_login,
            create_home,
            allow_groups,
        } => {
            let opts = JoinOptions {
                manual_login: *manual_login,
                create_home: *create_home,
                login_access: LoginAccess::from_groups(allow_groups),
                ..JoinOptions::default()
            };
            let stream = signal_stream(&proxy).await?;
            let job = proxy.apply_options(&serde_json::to_string(&opts)?).await?;
            println!("Job {job} started.");
            let result = wait_for_job(&proxy, stream, job, cli.json).await;
            if !result.ok {
                std::process::exit(1);
            }
        }
        Command::Watch => {
            println!("Watching signals from the helper (Ctrl-C to stop)…");
            let stream = signal_stream(&proxy).await?;
            let mut stream = std::pin::pin!(stream);
            while let Some(sig) = stream.next().await {
                match sig {
                    Signal::Progress(j, ev) | Signal::Log(j, ev) => {
                        print!("job {j}: ");
                        print_event(&ev);
                    }
                    Signal::Finished(j, result) => {
                        println!(
                            "job {j} finished: {}",
                            if result.ok { "ok" } else { "failed" }
                        );
                        println!("{}", result.summary);
                    }
                }
            }
        }
    }
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let cli = Cli::parse();
    let outcome: Result<(), String> = if cli.direct {
        run_direct(&cli)
    } else {
        run_dbus(&cli).await.map_err(|e| e.to_string())
    };
    if let Err(e) = outcome {
        eprintln!("rabitactl: {e}");
        if !cli.direct && e.contains("org.quietbyte.Rabita1") {
            eprintln!("hint: is rabita-helper running? Check: busctl status org.quietbyte.Rabita1");
        }
        std::process::exit(2);
    }
}

#[allow(dead_code)]
fn _assert_runner_is_shareable() {
    let _: Arc<dyn rabita_helper::cmd::Runner> = Arc::new(SystemRunner);
}
