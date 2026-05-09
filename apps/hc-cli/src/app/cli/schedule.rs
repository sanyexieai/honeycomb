//! `hc-cli schedule` 子命令。
use anyhow::{Context, Result, bail};
use hc_conversation::FollowUpStatus;
use hc_protocol::ApiNamespace;
use hc_scheduler::{ScheduleSpec, ScheduleStatus, ScheduledTarget, ScheduledTask, now_unix};
use hc_service::ServiceConfig;
use hc_service::scheduler::{
    SchedulerDispatchReceipt, SchedulerDispatchReport, dispatch_due_scheduled_runs,
    dispatch_queued_scheduled_runs,
};
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
        [cmd, rest @ ..] if cmd == "followups" => handle_schedule_followups(rest),
        [cmd, rest @ ..] if cmd == "stats" => handle_schedule_stats(rest),
        [] => bail!(
            "usage: hc-cli schedule <add|list|run-due|runs|pause|resume|dispatch-due|dispatch-queued|watch|followups|stats> ..."
        ),
        [other, ..] => bail!("unknown schedule command: {other}"),
    }
}

fn handle_schedule_followups(args: &[String]) -> Result<()> {
    match args {
        [sub, rest @ ..] if sub == "list" => handle_schedule_followups_list(rest),
        [sub, rest @ ..] if sub == "cancel" => handle_schedule_followups_cancel(rest),
        [sub, rest @ ..] if sub == "replay-events" => handle_schedule_followups_replay_events(rest),
        [] => bail!("usage: hc-cli schedule followups <list|cancel|replay-events> ..."),
        [other, ..] => bail!("unknown schedule followups command: {other}"),
    }
}

fn handle_schedule_followups_replay_events(args: &[String]) -> Result<()> {
    let mut since_created = 0u64;
    let mut json = false;
    let mut print_stdout = true;
    let mut index = 0usize;
    while index < args.len() {
        match args[index].as_str() {
            "--since-created-unix" => {
                since_created = super::parse_u64_arg(
                    args.get(index + 1)
                        .context("missing value for --since-created-unix")?,
                    "--since-created-unix",
                )?;
                index += 2;
            }
            "--json" => {
                json = true;
                index += 1;
            }
            "--no-print" => {
                print_stdout = false;
                index += 1;
            }
            other => bail!("unexpected schedule followups replay-events argument: {other}"),
        }
    }

    let config = ServiceConfig::from_env();
    let rows = hc_service::scheduler::list_timed_followup_fired_events_since_created(
        &config,
        &super::runtime_namespace(),
        since_created,
    )?;

    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }

    println!(
        "schedule> timed.followup.fired rows={} since_created_unix>={}",
        rows.len(),
        since_created
    );
    if print_stdout {
        for row in &rows {
            match row.draft_message.as_deref() {
                Some(msg) if !msg.trim().is_empty() => println!("assistant> {}", msg.trim()),
                _ => println!(
                    "schedule> followup_id={} event={} created_at_unix={} (no draft_message)",
                    row.followup_id, row.event_id, row.created_at_unix
                ),
            }
        }
    }
    Ok(())
}

fn handle_schedule_followups_list(args: &[String]) -> Result<()> {
    let mut json = false;
    let mut status_filter: Option<FollowUpStatus> = None;
    let mut due_only = false;
    let mut index = 0usize;
    while index < args.len() {
        match args[index].as_str() {
            "--json" => {
                json = true;
                index += 1;
            }
            "--status" => {
                let raw = args.get(index + 1).context("missing value for --status")?;
                status_filter = Some(parse_followup_status_filter(raw)?);
                index += 2;
            }
            "--due-only" => {
                due_only = true;
                index += 1;
            }
            other => bail!("unexpected schedule followups list argument: {other}"),
        }
    }

    let config = ServiceConfig::from_env();
    let items =
        hc_service::scheduler::list_conversation_followups(&config, &super::runtime_namespace())?;
    let now = now_unix();
    let filtered: Vec<_> = items
        .into_iter()
        .filter(|f| {
            if let Some(s) = status_filter {
                if f.status != s {
                    return false;
                }
            }
            if due_only && !(f.status == FollowUpStatus::Pending && f.due_at_unix <= now) {
                return false;
            }
            true
        })
        .collect();

    if json {
        println!("{}", serde_json::to_string_pretty(&filtered)?);
        return Ok(());
    }
    if filtered.is_empty() {
        println!("schedule> no follow-ups match");
        return Ok(());
    }
    for f in filtered {
        let idem = f
            .payload
            .get("timed_run_idempotency_key_v1")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("-");
        println!(
            "{} | {:?} | due_at_unix={} | trigger={} | idem={}",
            f.id, f.status, f.due_at_unix, f.trigger, idem
        );
    }
    Ok(())
}

fn parse_followup_status_filter(raw: &str) -> Result<FollowUpStatus> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "pending" => Ok(FollowUpStatus::Pending),
        "fired" => Ok(FollowUpStatus::Fired),
        "cancelled" => Ok(FollowUpStatus::Cancelled),
        "failed" => Ok(FollowUpStatus::Failed),
        other => bail!("unknown follow-up status filter: {other}"),
    }
}

fn handle_schedule_followups_cancel(args: &[String]) -> Result<()> {
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
            other => bail!("unexpected schedule followups cancel argument: {other}"),
        }
    }
    let followup_id = id.context("missing --id")?;
    let config = ServiceConfig::from_env();
    let followup = hc_service::scheduler::cancel_followup_with_timed_mirror(
        &config,
        &super::runtime_namespace(),
        &followup_id,
    )?;
    if json {
        println!("{}", serde_json::to_string_pretty(&followup)?);
    } else {
        println!(
            "schedule> cancelled follow-up {} ({:?})",
            followup.id, followup.status
        );
    }
    Ok(())
}

fn handle_schedule_stats(args: &[String]) -> Result<()> {
    let mut json = false;
    let mut now = None::<u64>;
    let mut index = 0usize;
    while index < args.len() {
        match args[index].as_str() {
            "--json" => {
                json = true;
                index += 1;
            }
            "--now-unix" => {
                now = Some(super::parse_u64_arg(
                    args.get(index + 1)
                        .context("missing value for --now-unix")?,
                    "--now-unix",
                )?);
                index += 2;
            }
            other => bail!("unexpected schedule stats argument: {other}"),
        }
    }
    let config = ServiceConfig::from_env();
    let ns = super::runtime_namespace();
    let stats = hc_service::scheduler::scheduler_operational_stats(&config, &ns, now)?;
    let stats =
        hc_service::scheduler::merge_scheduler_operational_stats_with_dispatch_slip_histogram(
            stats,
            &ns.tenant_id,
            &ns.user_id,
        );
    if json {
        println!("{}", serde_json::to_string_pretty(&stats)?);
    } else {
        println!("schedule> stats now_unix={}", stats.now_unix);
        println!(
            "  followups: total={} pending={} pending_due_now={} fired={} cancelled={} failed={}",
            stats.followup_total,
            stats.followup_pending,
            stats.followup_pending_due,
            stats.followup_fired,
            stats.followup_cancelled,
            stats.followup_failed
        );
        println!(
            "  schedules: total={} active={} paused={} cancelled={} timed_mirror_active={}",
            stats.schedule_total,
            stats.schedule_active,
            stats.schedule_paused,
            stats.schedule_cancelled,
            stats.schedule_timed_mirror_active
        );
        println!(
            "  runs: queued={} running={} succeeded={} failed={} cancelled={}",
            stats.run_queued,
            stats.run_running,
            stats.run_succeeded,
            stats.run_failed,
            stats.run_cancelled
        );
        println!(
            "  note: hc-api-only api_* fields (e.g. api_followup_messages_delivered_total) are absent here; slip histogram scheduled_run_dispatch_slip_ms_histogram reflects this process after dispatch / watch."
        );
    }
    Ok(())
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

    let report = cli_dispatch_due(now)?;
    print_schedule_dispatch_receipts(report.receipts, json)
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
        let report = cli_dispatch_due(now)?;
        if json {
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "now_unix": report.now_unix,
                    "queued_count": report.queued_count,
                    "receipts": report.receipts,
                }))?
            );
        } else if report.receipts.is_empty() {
            println!("schedule> tick now={} no due runs", report.now_unix);
        } else {
            println!(
                "schedule> tick now={} dispatched={}",
                report.now_unix,
                report.receipts.len()
            );
            for receipt in &report.receipts {
                println!("dispatch> {} status={}", receipt.run_id, receipt.status);
            }
            print_timed_followup_messages(&report.receipts)?;
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

fn cli_dispatch_due(now: u64) -> Result<SchedulerDispatchReport> {
    let config = ServiceConfig::from_env();
    let namespace: ApiNamespace = super::runtime_namespace().into();
    dispatch_due_scheduled_runs(&config, namespace, Some(now))
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

    let report = cli_dispatch_queued(now)?;
    print_schedule_dispatch_receipts(report.receipts, json)
}

fn cli_dispatch_queued(now: u64) -> Result<SchedulerDispatchReport> {
    let config = ServiceConfig::from_env();
    let namespace: ApiNamespace = super::runtime_namespace().into();
    dispatch_queued_scheduled_runs(&config, namespace, Some(now))
}

fn print_schedule_dispatch_receipts(
    receipts: Vec<SchedulerDispatchReceipt>,
    json: bool,
) -> Result<()> {
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
