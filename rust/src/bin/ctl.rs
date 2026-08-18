use opencodeserver::VERSION;
use opencodeserver::ipc::send_request;
use opencodeserver::paths::AppPaths;
use opencodeserver::protocol::{Command, Request, Response, ServerState, Status};
use std::process::{Command as ProcessCommand, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn main() {
    if let Err(error) = run() {
        eprintln!("opencodeserverctl: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = arguments.first().map(String::as_str) else {
        print_usage();
        return Err("a command is required".into());
    };
    let paths = AppPaths::discover()?;
    match command {
        "status" => {
            let json = arguments.iter().any(|argument| argument == "--json");
            ensure_only_flags(&arguments[1..], &["--json"])?;
            let response = request(&paths, Command::Status)?;
            let status = response_status(&response)?;
            if json {
                println!("{}", serde_json::to_string(status)?);
            } else {
                print_status(status);
            }
        }
        "start" => {
            ensure_only_flags(&arguments[1..], &[])?;
            let response = request(&paths, Command::Start)?;
            print_status(response_status(&response)?);
        }
        "stop" => {
            ensure_only_flags(&arguments[1..], &["--force"])?;
            let force = arguments.iter().any(|argument| argument == "--force");
            request(&paths, Command::Stop)?;
            wait_for_stop(&paths, force)?;
        }
        "restart" => {
            ensure_only_flags(&arguments[1..], &["--force"])?;
            let force = arguments.iter().any(|argument| argument == "--force");
            request(&paths, Command::Restart)?;
            wait_for_restart(&paths, force)?;
        }
        "logs" => {
            ensure_only_flags(&arguments[1..], &[])?;
            let status = ProcessCommand::new("/usr/bin/log")
                .args([
                    "show",
                    "--style",
                    "compact",
                    "--last",
                    "1h",
                    "--predicate",
                    "subsystem == \"ai.opencode.server\"",
                ])
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status()?;
            if !status.success() {
                return Err(format!("log exited with {status}").into());
            }
        }
        "version" => {
            ensure_only_flags(&arguments[1..], &[])?;
            println!("opencodeserverctl {VERSION}");
        }
        "validate-config" => {
            ensure_only_flags(&arguments[1..], &["--json"])?;
            let json = arguments.iter().any(|argument| argument == "--json");
            let response = send_request(&paths, &Request::new(Command::ValidateConfig))?;
            let report = response
                .validation
                .ok_or("OpenCodeServerAgent returned no validation report")?;
            if json {
                println!("{}", serde_json::to_string(&report)?);
            } else if report.valid {
                println!(
                    "Configuration is valid. OpenCode: {}",
                    report.selected_executable.as_deref().unwrap_or("automatic")
                );
            } else {
                for issue in &report.issues {
                    eprintln!("Invalid: {issue}");
                }
                return Err("configuration is invalid".into());
            }
        }
        "help" | "--help" | "-h" => print_usage(),
        unknown => return Err(format!("unknown command: {unknown}").into()),
    }
    Ok(())
}

fn request(paths: &AppPaths, command: Command) -> Result<Response, Box<dyn std::error::Error>> {
    let response = send_request(paths, &Request::new(command))?;
    if response.ok {
        Ok(response)
    } else {
        Err(response
            .error
            .unwrap_or_else(|| "OpenCodeServerAgent rejected the request".to_owned())
            .into())
    }
}

fn response_status(response: &Response) -> Result<&Status, Box<dyn std::error::Error>> {
    response
        .status
        .as_ref()
        .ok_or_else(|| "OpenCodeServerAgent returned no status".into())
}

fn wait_for_stop(paths: &AppPaths, force: bool) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = Instant::now() + Duration::from_secs(18);
    loop {
        let response = request(paths, Command::Status)?;
        let status = response_status(&response)?;
        if status.server_state == ServerState::Stopped {
            print_status(status);
            return Ok(());
        }
        if status.server_state == ServerState::StopTimedOut {
            if force {
                request(paths, Command::ForceStop)?;
            } else {
                print_status(status);
                return Err("graceful stop timed out; use `stop --force` or retry later".into());
            }
        }
        if Instant::now() >= deadline {
            return Err(
                "timed out waiting for OpenCodeServerAgent to finish stopping OpenCode".into(),
            );
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn wait_for_restart(paths: &AppPaths, force: bool) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = Instant::now() + Duration::from_secs(35);
    loop {
        let response = request(paths, Command::Status)?;
        let status = response_status(&response)?;
        if status.server_state == ServerState::Healthy {
            print_status(status);
            return Ok(());
        }
        if status.server_state == ServerState::StopTimedOut {
            if force {
                request(paths, Command::ForceStop)?;
            } else {
                print_status(status);
                return Err(
                    "restart is waiting for graceful stop; use `restart --force` if needed".into(),
                );
            }
        }
        if status.server_state == ServerState::Failed {
            print_status(status);
            return Err("restart failed".into());
        }
        if Instant::now() >= deadline {
            print_status(status);
            return Err("timed out waiting for a healthy restarted OpenCode".into());
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn print_status(status: &Status) {
    println!("OpenCode:           {}", state_label(status.server_state));
    println!("Endpoint:           {}", status.endpoint);
    println!(
        "OpenCode:           running {} / installed {}",
        status.running_version.as_deref().unwrap_or("unknown"),
        status.installed_version.as_deref().unwrap_or("unknown")
    );
    println!("Full Disk Access:   {:?}", status.fda);
    println!("Username:           {}", status.username);
    println!(
        "Authentication:     {}",
        if status.authentication_enabled {
            "Configured"
        } else {
            "Not enabled"
        }
    );
    if let Some(seconds) = status.uptime_seconds {
        println!("Uptime:             {}s", seconds);
    }
    if status.config_pending {
        println!("Configuration:      Pending restart");
    }
    if let Some(error) = status
        .config_error
        .as_deref()
        .or(status.last_error.as_deref())
    {
        println!("Detail:             {error}");
    }
}

fn state_label(state: ServerState) -> &'static str {
    match state {
        ServerState::Stopped => "Stopped",
        ServerState::Starting => "Starting",
        ServerState::Healthy => "Healthy",
        ServerState::Unhealthy => "Running, unhealthy",
        ServerState::Stopping => "Stopping",
        ServerState::StopTimedOut => "Waiting after graceful timeout",
        ServerState::WaitingToRestart => "Waiting to restart",
        ServerState::Failed => "Failed",
    }
}

fn ensure_only_flags(
    arguments: &[String],
    allowed: &[&str],
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(argument) = arguments
        .iter()
        .find(|argument| !allowed.contains(&argument.as_str()))
    {
        Err(format!("unexpected argument: {argument}").into())
    } else {
        Ok(())
    }
}

fn print_usage() {
    println!(
        "Usage: opencodeserverctl <status [--json] | start | stop [--force] | restart [--force] | logs | version | validate-config [--json]>"
    );
}
