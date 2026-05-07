//! `hc-cli schedule` 子命令。
use anyhow::{Context, Result, bail};
use hc_scheduler::{
    ScheduleSpec, ScheduleStatus, ScheduledRun, ScheduledTarget, ScheduledTask, now_unix,
};
use hc_service::ServiceConfig;
use hc_service::scheduler::SchedulerDispatchReceipt;
use std::time::Duration;

pub(super) fn handle_schedule(args: &[String]) -> Result<()> {
    match args {
        [cmd, rest @ ..] if cmd == "add" => handle_schedule_add(rest),
        [cmd, rest @ ..] if cmd == "list" => handle_schedule_list(rest),
        [cmd, rest @ ..] if cmd == "run-due" => handle_schedule_run_due(rest),
        [cmd, rest @ ..] if cmd == "runs" => handle_schedule_runs(rest),
        [cmd, rest @ ..] if cmd == "pause" => {
            handle_schedule_set_status(rest, ScheduleStatus::Paused)
        }
        [cmd, rest @ ..] if cmd == "resume" => {
            handle_schedule_set_status(rest, ScheduleStatus::Active)
        }
        [cmd, rest @ ..] if cmd == "dispatch-due" => handle_schedule_dispatch_due(rest),
        [cmd, rest @ ..] if cmd == "dispatch-queued" => handle_schedule_dispatch_queued(rest),
        [cmd, rest @ ..] if cmd == "watch" => handle_schedule_watch(rest),
        [] => bail!(
            "usage: hc-cli schedule <add|list|run-due|runs|pause|resume|dispatch-due|dispatch-queued|watch> ..."
        ),
        [other, ..] => bail!("unknown schedule command: {other}"),
    }
}

fn handle_schedule_add(args: &[String]) -> Result<()> {
    let mut id = None;
    let mut title = None;
    let mut kind = None;
    let mut run_at_unix = None;
    let mut interval_seconds = None;
    let mut target_kind = None;
    let mut target_ref = None;
    let mut target_action = None;
    let mut target_args = serde_json::Map::new();
    let mut tags = Vec::new();
    let mut json = false;

    let mut index = 0usize;
    while index < args.len() {
        match args[index].as_str() {
            "--id" => {
                id = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --id")?,
                );
                index += 2;
            }
            "--title" => {
                title = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --title")?,
                );
                index += 2;
            }
            "--kind" => {
                kind = Some(super::parse_schedule_kind(
                    args.get(index + 1).context("missing value for --kind")?,
                )?);
                index += 2;
            }
            "--run-at-unix" => {
                run_at_unix = Some(super::parse_u64_arg(
                    args.get(index + 1)
                        .context("missing value for --run-at-unix")?,
                    "--run-at-unix",
                )?);
                index += 2;
            }
            "--interval-seconds" => {
                interval_seconds = Some(super::parse_u64_arg(
                    args.get(index + 1)
                        .context("missing value for --interval-seconds")?,
                    "--interval-seconds",
                )?);
                index += 2;
            }
            "--target-kind" => {
                target_kind = Some(super::parse_scheduled_target_kind(
                    args.get(index + 1)
                        .context("missing value for --target-kind")?,
                )?);
                index += 2;
            }
            "--target-ref" => {
                target_ref = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --target-ref")?,
                );
                index += 2;
            }
            "--target-action" => {
                target_action = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --target-action")?,
                );
                index += 2;
            }
            "--arg" => {
                let arg = args.get(index + 1).context("missing value for --arg")?;
                let (key, value) = arg
                    .split_once('=')
                    .with_context(|| format!("schedule --arg must use key=value form: {arg}"))?;
                target_args.insert(key.to_owned(), super::parse_jsonish_argument_value(value));
                index += 2;
            }
            "--tag" => {
                tags.push(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --tag")?,
                );
                index += 2;
            }
            "--json" => {
                json = true;
                index += 1;
            }
            other => bail!("unexpected schedule add argument: {other}"),
        }
    }

    let task = ScheduledTask::new(
        id.context("missing --id")?,
        title.context("missing --title")?,
        ScheduleSpec {
            kind: kind.context("missing --kind")?,
            run_at_unix,
            interval_seconds,
        },
        ScheduledTarget {
            kind: target_kind.context("missing --target-kind")?,
            r#ref: target_ref.context("missing --target-ref")?,
            action: target_action,
            args: target_args,
        },
    );
    let mut task = task;
    if !tags.is_empty() {
        task.tags = super::normalized_tags(tags, "scheduled");
    }
    let path = super::schedule_repository().write_schedule(&task)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schedule": task,
                "path": path,
            }))?
        );
    } else {
        println!("schedule> {}", task.id);
        println!("path> {}", path.display());
        println!("next_fire_at_unix> {:?}", task.state.next_fire_at_unix);
    }
    Ok(())
}

fn handle_schedule_list(args: &[String]) -> Result<()> {
    let options = super::parse_common_options(args)?;
    let schedules = super::schedule_repository().list_schedules()?;
    if options.json {
        println!("{}", serde_json::to_string_pretty(&schedules)?);
        return Ok(());
    }
    for schedule in schedules {
        println!(
            "{} | {:?} | next={:?} | {:?}:{}",
            schedule.id,
            schedule.status,
            schedule.state.next_fire_at_unix,
            schedule.target.kind,
            schedule.target.r#ref
        );
    }
    Ok(())
}

fn handle_schedule_run_due(args: &[String]) -> Result<()> {
    let mut now = now_unix();
    let mut json = false;
    let mut index = 0usize;
    while index < args.len() {
        match args[index].as_str() {
            "--now-unix" => {
                now = super::parse_u64_arg(
                    args.get(index + 1)
                        .context("missing value for --now-unix")?,
                    "--now-unix",
                )?;
                index += 2;
            }
            "--json" => {
                json = true;
                index += 1;
            }
            other => bail!("unexpected schedule run-due argument: {other}"),
        }
    }
    let runs = super::schedule_repository().queue_due_runs(now)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&runs)?);
        return Ok(());
    }
    if runs.is_empty() {
        println!("schedule> no due runs");
    } else {
        for run in runs {
            println!(
                "run> {} schedule={} target={:?}:{}",
                run.id, run.schedule_id, run.target.kind, run.target.r#ref
            );
        }
    }
    Ok(())
}

fn handle_schedule_runs(args: &[String]) -> Result<()> {
    let options = super::parse_common_options(args)?;
    let runs = super::schedule_repository().list_runs()?;
    if options.json {
        println!("{}", serde_json::to_string_pretty(&runs)?);
        return Ok(());
    }
    for run in runs {
        println!(
            "{} | schedule={} | {:?} | target={:?}:{}",
            run.id, run.schedule_id, run.status, run.target.kind, run.target.r#ref
        );
    }
    Ok(())
}

fn handle_schedule_set_status(args: &[String], status: ScheduleStatus) -> Result<()> {
    let mut id = None;
    let mut json = false;
    let mut index = 0usize;
    while index < args.len() {
        match args[index].as_str() {
            "--id" => {
                id = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --id")?,
                );
                index += 2;
            }
            "--json" => {
                json = true;
                index += 1;
            }
            other => bail!("unexpected schedule status argument: {other}"),
        }
    }
    let task =
        super::schedule_repository().set_schedule_status(&id.context("missing --id")?, status)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&task)?);
    } else {
        println!("schedule> {} {:?}", task.id, task.status);
    }
    Ok(())
}

fn handle_schedule_dispatch_due(args: &[String]) -> Result<()> {
    let mut now = now_unix();
    let mut json = false;
    let mut index = 0usize;
    while index < args.len() {
        match args[index].as_str() {
            "--now-unix" => {
                now = super::parse_u64_arg(
                    args.get(index + 1)
                        .context("missing value for --now-unix")?,
                    "--now-unix",
                )?;
                index += 2;
            }
            "--json" => {
                json = true;
                index += 1;
            }
            other => bail!("unexpected schedule dispatch-due argument: {other}"),
        }
    }

    let receipts = dispatch_due_scheduled_runs(now)?;
    print_schedule_dispatch_receipts(receipts, json)
}

fn handle_schedule_watch(args: &[String]) -> Result<()> {
    let mut tick_seconds = 30u64;
    let mut max_ticks = None;
    let mut json = false;
    let mut index = 0usize;
    while index < args.len() {
        match args[index].as_str() {
            "--tick-seconds" => {
                tick_seconds = super::parse_u64_arg(
                    args.get(index + 1)
                        .context("missing value for --tick-seconds")?,
                    "--tick-seconds",
                )?;
                index += 2;
            }
            "--max-ticks" => {
                max_ticks = Some(super::parse_u64_arg(
                    args.get(index + 1)
                        .context("missing value for --max-ticks")?,
                    "--max-ticks",
                )?);
                index += 2;
            }
            "--json" => {
                json = true;
                index += 1;
            }
            other => bail!("unexpected schedule watch argument: {other}"),
        }
    }
    if tick_seconds == 0 {
        bail!("--tick-seconds must be > 0");
    }

    let mut ticks = 0u64;
    loop {
        let now = now_unix();
        let receipts = dispatch_due_scheduled_runs(now)?;
        if json {
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "now_unix": now,
                    "receipts": receipts,
                }))?
            );
        } else if receipts.is_empty() {
            println!("schedule> tick now={} no due runs", now);
        } else {
            println!("schedule> tick now={} dispatched={}", now, receipts.len());
            for receipt in &receipts {
                println!(
                    "dispatch> {} status={}",
                    receipt.run_id,
                    receipt.status
                );
            }
            print_timed_followup_messages(&receipts)?;
        }
        ticks += 1;
        if max_ticks.is_some_and(|limit| ticks >= limit) {
            break;
        }
        std::thread::sleep(Duration::from_secs(tick_seconds));
    }
    Ok(())
}

fn print_timed_followup_messages(receipts: &[SchedulerDispatchReceipt]) -> Result<()> {
    struct StdoutFollowUpSink;
    impl hc_service::scheduler::FollowUpMessageSink for StdoutFollowUpSink {
        fn on_fired_followup_message(
            &mut self,
            message: &hc_service::scheduler::FiredFollowUpMessage,
        ) {
            println!("assistant> {}", message.message);
        }
    }

    if receipts.is_empty() {
        return Ok(());
    }
    let config = ServiceConfig::from_env();
    let mut sink = StdoutFollowUpSink;
    hc_service::scheduler::dispatch_fired_followup_messages_from_receipts(
        &config,
        super::runtime_namespace().into(),
        receipts,
        &mut sink,
    )?;
    Ok(())
}

fn dispatch_due_scheduled_runs(now: u64) -> Result<Vec<SchedulerDispatchReceipt>> {
    let mut receipts = Vec::new();
    super::schedule_repository().queue_due_runs(now)?;
    for run in super::schedule_repository().queued_runs()? {
        receipts.push(dispatch_scheduled_run(run, now)?);
    }
    Ok(receipts)
}

fn handle_schedule_dispatch_queued(args: &[String]) -> Result<()> {
    let mut now = now_unix();
    let mut json = false;
    let mut index = 0usize;
    while index < args.len() {
        match args[index].as_str() {
            "--now-unix" => {
                now = super::parse_u64_arg(
                    args.get(index + 1)
                        .context("missing value for --now-unix")?,
                    "--now-unix",
                )?;
                index += 2;
            }
            "--json" => {
                json = true;
                index += 1;
            }
            other => bail!("unexpected schedule dispatch-queued argument: {other}"),
        }
    }

    let mut receipts = Vec::new();
    for run in super::schedule_repository().queued_runs()? {
        receipts.push(dispatch_scheduled_run(run, now)?);
    }
    print_schedule_dispatch_receipts(receipts, json)
}

fn print_schedule_dispatch_receipts(receipts: Vec<SchedulerDispatchReceipt>, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(&receipts)?);
        return Ok(());
    }
    if receipts.is_empty() {
        println!("schedule> no due runs");
    } else {
        for receipt in receipts {
            println!(
                "dispatch> {} target={}:{} status={} result={}",
                receipt.run_id,
                receipt.target_kind,
                receipt.target_ref,
                receipt.status,
                receipt.result_ref.as_deref().unwrap_or("")
            );
        }
    }
    Ok(())
}

fn dispatch_scheduled_run(run: ScheduledRun, now: u64) -> Result<SchedulerDispatchReceipt> {
    let config = ServiceConfig::from_env();
    let namespace = super::runtime_namespace();
    let repository = super::schedule_repository();
    hc_service::scheduler::dispatch_scheduled_run(&config, &namespace, &repository, run, now)
}
