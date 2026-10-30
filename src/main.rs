use clap::Parser;
use git2::{IndexAddOption, Repository};
use rand::RngExt;
use std::{
    io,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Parser, Debug)]
#[command(name = "git-auto-committer")]
#[command(about = "Batch auto-commits for Git history", long_about = None)]
struct Args {
    /// Path to git repository (default: current directory)
    #[arg(short, long, default_value = ".")]
    repo_path: String,

    /// Number of commits per batch
    #[arg(short = 'n', long, default_value = "1000", value_parser = clap::value_parser!(u32).range(1..))]
    batch_size: u32,

    /// Commit message prefix
    #[arg(short = 'm', long, default_value = "Auto commit")]
    message_prefix: String,

    /// Delay between batches in seconds
    #[arg(short, long, default_value = "1")]
    batch_delay: u64,

    /// Total commits to make (0 = infinite)
    #[arg(short, long, default_value = "0")]
    total: u32,

    /// Number of new commits between pushes
    #[arg(long, default_value = "1000", value_parser = clap::value_parser!(u64).range(1..))]
    push_every: u64,

    /// Maximum additional attempts after transient push failures
    #[arg(long, default_value = "3")]
    push_retries: u32,

    /// Delay between push attempts in seconds
    #[arg(long, default_value = "2")]
    push_retry_delay: u64,

    /// Remote to push the current branch to
    #[arg(long, default_value = "origin")]
    push_remote: String,
}

fn prepare_tree(repo: &Repository) -> Result<git2::Tree<'_>, git2::Error> {
    let mut index = repo.index()?;
    index.add_all(["*"], IndexAddOption::DEFAULT, None)?;
    repo.find_tree(index.write_tree()?)
}

fn transient_push_failure(stderr: &str) -> bool {
    let message = stderr.to_ascii_lowercase();
    [
        "could not resolve",
        "failed to connect",
        "connection reset",
        "connection refused",
        "connection timed out",
        "operation timed out",
        "remote end hung up",
        "unexpected disconnect",
        "http 500",
        "http 502",
        "http 503",
        "http 504",
        "returned error: 500",
        "returned error: 502",
        "returned error: 503",
        "returned error: 504",
    ]
    .iter()
    .any(|pattern| message.contains(pattern))
}

fn push_current_branch(repo: &Repository, args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let head = repo.head()?;
    if !head.is_branch() {
        return Err(io::Error::other("Auto-push requires a checked-out branch").into());
    }
    let branch_ref = head.name()?;
    // Use native Git to preserve configured credential helpers, SSH and proxy authentication.
    for attempt in 0..=args.push_retries {
        let mut command = Command::new("git");
        // Let an in-flight push finish when Ctrl+C requests a graceful shutdown.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let output = command
            .arg("-C")
            .arg(repo.workdir().unwrap_or(repo.path()))
            .args([
                "push",
                "--",
                &args.push_remote,
                &format!("HEAD:{branch_ref}"),
            ])
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprint!("{stderr}");
        if output.status.success() {
            return Ok(());
        }
        if attempt == args.push_retries || !transient_push_failure(&stderr) {
            return Err(io::Error::other(format!(
                "Auto-push failed ({}); local commits are preserved",
                output.status
            ))
            .into());
        }
        eprintln!(
            "Transient push failure; retry {}/{} in {}s",
            attempt + 1,
            args.push_retries,
            args.push_retry_delay
        );
        std::thread::sleep(Duration::from_secs(args.push_retry_delay));
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let repo = Repository::open(&args.repo_path)?;
    let stopped = Arc::new(AtomicBool::new(false));
    let stop_flag = Arc::clone(&stopped);
    ctrlc::set_handler(move || stop_flag.store(true, Ordering::Relaxed))?;
    let mut pending = 0u64;
    let mut rng = rand::rng();

    let mut total_commits = 0u64;
    let mut batch_count = 0u64;

    println!("Starting auto-committer...");
    println!("Batch size: {}", args.batch_size);
    println!("Repository: {}", args.repo_path);
    println!("---");

    loop {
        if stopped.load(Ordering::Relaxed)
            || (args.total > 0 && total_commits >= u64::from(args.total))
        {
            println!("\nCompleted {} commits total!", total_commits);
            break;
        }

        let batch_start = Instant::now();
        let commits_in_batch = if args.total > 0 {
            u64::from(args.batch_size).min(u64::from(args.total) - total_commits)
        } else {
            u64::from(args.batch_size)
        };

        // Stage and write the tree once per batch; commits reuse the same snapshot.
        let tree = prepare_tree(&repo)?;
        let signature = repo.signature()?;
        let mut parent = match repo.head() {
            Ok(head) => Some(head.peel_to_commit()?),
            Err(error) if error.code() == git2::ErrorCode::UnbornBranch => None,
            Err(error) => return Err(error.into()),
        };
        let mut completed = 0u64;
        for i in 0..commits_in_batch {
            if stopped.load(Ordering::Relaxed) {
                break;
            }
            let msg = format!("{} #{}", args.message_prefix, rng.random::<u32>());
            let parents: Vec<_> = parent.iter().collect();
            let oid = repo.commit(Some("HEAD"), &signature, &signature, &msg, &tree, &parents)?;
            parent = Some(repo.find_commit(oid)?);
            completed += 1;
            total_commits += 1;
            pending += 1;
            if pending >= args.push_every {
                println!("\nPushing after {total_commits} commits...");
                push_current_branch(&repo, &args)?;
                pending = 0;
            }
            if (i + 1) % 100 == 0 {
                print!(".");
            }
        }

        let batch_time = batch_start.elapsed();
        batch_count += 1;

        println!(
            "\nBatch {} done: {} commits in {:?} ({:.1} comm/s)",
            batch_count,
            completed,
            batch_time,
            completed as f64 / batch_time.as_secs_f64()
        );

        if (args.total == 0 || total_commits < u64::from(args.total)) && args.batch_delay > 0 {
            let deadline = Instant::now() + Duration::from_secs(args.batch_delay);
            while !stopped.load(Ordering::Relaxed) && Instant::now() < deadline {
                std::thread::sleep(
                    deadline
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(100)),
                );
            }
        }
    }

    if pending > 0 {
        println!("Pushing remaining {pending} commits...");
        push_current_branch(&repo, &args)?;
    }
    println!(
        "\nTotal: {} commits in {} batches",
        total_commits, batch_count
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_zero_batch_size() {
        let error = Args::try_parse_from(["git-auto-committer", "--batch-size", "0"])
            .expect_err("a zero batch would never make progress");
        assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
    }

    #[test]
    fn accepts_smallest_batch_and_finite_total() {
        let args =
            Args::try_parse_from(["git-auto-committer", "--batch-size", "1", "--total", "3"])
                .unwrap();
        assert_eq!(args.batch_size, 1);
        assert_eq!(args.total, 3);
    }
}
