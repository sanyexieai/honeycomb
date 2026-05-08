//! `hc-cli room` 子命令（memory room 管理）。
use anyhow::{Context, Result, bail};
use hc_bootstrap::workspace_root;
use hc_context::MemoryNamespace;
use hc_memory::{
    CapabilityRef, InheritanceType, MemoryNamespace as MemoryNamespaceStorage,
    MemoryRoom as MemoryRoomStorage, MemoryRoomRepository, RoomCapabilityResolver, ScheduleRef,
    SkillRef, ToolRef,
};
use serde_json;

use super::{CLI_RUNTIME_CONTEXT, parse_common_options, runtime_namespace};

// Room 能力管理命令
pub(super) fn handle_room(args: &[String]) -> Result<()> {
    match args {
        [cmd, rest @ ..] if cmd == "create" => handle_room_create(rest),
        [cmd, rest @ ..] if cmd == "list" => handle_room_list(rest),
        [cmd, rest @ ..] if cmd == "show" => handle_room_show(rest),
        [cmd, rest @ ..] if cmd == "capabilities" => handle_room_capabilities(rest),
        [cmd, rest @ ..] if cmd == "inherit" => handle_room_inherit(rest),
        [] => {
            println!("room commands:");
            println!("  create    - create a new memory room");
            println!("  list      - list memory rooms");
            println!("  show      - show room details");
            println!("  capabilities - show room capabilities");
            println!("  inherit   - manage capability inheritance");
            Ok(())
        }
        [other, ..] => bail!("unknown room command: {other}"),
    }
}

pub(super) fn handle_room_create(args: &[String]) -> Result<()> {
    let mut id = None;
    let mut layer = None;
    let mut title = None;
    let mut summary = None;
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
            "--layer" => {
                layer = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --layer")?,
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
            "--summary" => {
                summary = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --summary")?,
                );
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
            arg => bail!("unknown argument: {arg}"),
        }
    }

    let id = id.context("missing --id")?;
    let layer = layer.context("missing --layer")?;
    let title = title.context("missing --title")?;
    let summary = summary.unwrap_or_else(|| format!("Memory room for {}", title));

    let memory_layer = match layer.as_str() {
        "chat" => hc_memory::MemoryLayer::Chat,
        "topic" => hc_memory::MemoryLayer::Topic,
        "task" => hc_memory::MemoryLayer::Task,
        "project" => hc_memory::MemoryLayer::Project,
        "global" => hc_memory::MemoryLayer::Global,
        _ => bail!(
            "invalid layer: {} (must be chat, topic, task, project, or global)",
            layer
        ),
    };

    let runtime_context = CLI_RUNTIME_CONTEXT.get().unwrap();
    let memory_namespace = MemoryNamespaceStorage::new(
        runtime_context.tenant_id.as_deref().unwrap_or("local"),
        runtime_context.user_id.as_deref().unwrap_or("default"),
    );
    let workspace_namespace = hc_store::store::WorkspaceNamespace::new(
        runtime_context.tenant_id.as_deref().unwrap_or("local"),
        runtime_context.user_id.as_deref().unwrap_or("default"),
    );

    let mut room =
        MemoryRoomStorage::new(id, memory_layer, title, summary).with_namespace(memory_namespace);

    for tag in tags {
        room = room.with_tag(tag);
    }

    let repository = MemoryRoomRepository::with_namespace(workspace_root(), workspace_namespace);
    let path = repository.write_room(&room)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&room)?);
    } else {
        println!("Created room {} at {}", room.id, path.display());
    }

    Ok(())
}

pub(super) fn handle_room_list(args: &[String]) -> Result<()> {
    let options = parse_common_options(args)?;
    let runtime_context = CLI_RUNTIME_CONTEXT.get().unwrap();
    let workspace_namespace = hc_store::store::WorkspaceNamespace::new(
        runtime_context.tenant_id.as_deref().unwrap_or("local"),
        runtime_context.user_id.as_deref().unwrap_or("default"),
    );

    let repository = MemoryRoomRepository::with_namespace(workspace_root(), workspace_namespace);

    let rooms = repository
        .list_rooms()
        .context("Failed to list memory rooms")?;

    if options.json {
        let room_data: Vec<_> = rooms
            .iter()
            .map(|room| {
                serde_json::json!({
                    "id": room.id,
                    "layer": format!("{:?}", room.layer).to_lowercase(),
                    "title": room.title,
                    "summary": room.summary,
                    "tags": room.tags,
                    "status": room.status,
                    "capabilities_count": room.inherited_capabilities.len(),
                    "tools_count": room.inherited_tools.len(),
                    "skills_count": room.inherited_skills.len(),
                    "schedules_count": room.inherited_schedules.len(),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&room_data)?);
    } else {
        if rooms.is_empty() {
            println!("No memory rooms found.");
        } else {
            println!("Memory Rooms ({} total):", rooms.len());
            println!();
            for room in rooms {
                println!("  {} ({:?})", room.id, room.layer);
                println!("    Title: {}", room.title);
                if !room.summary.is_empty() {
                    println!("    Summary: {}", room.summary);
                }
                if !room.status.is_empty() {
                    println!("    Status: {}", room.status);
                }

                if !room.tags.is_empty() {
                    println!("    Tags: {}", room.tags.join(", "));
                }

                let capabilities_count = room.inherited_capabilities.len()
                    + room.inherited_tools.len()
                    + room.inherited_skills.len()
                    + room.inherited_schedules.len();
                if capabilities_count > 0 {
                    println!(
                        "    Capabilities: {} capabilities, {} tools, {} skills, {} schedules",
                        room.inherited_capabilities.len(),
                        room.inherited_tools.len(),
                        room.inherited_skills.len(),
                        room.inherited_schedules.len()
                    );
                }
                println!();
            }
        }
    }

    Ok(())
}

pub(super) fn handle_room_show(args: &[String]) -> Result<()> {
    let mut room_id = None;
    let mut json = false;
    let mut index = 0usize;

    while index < args.len() {
        match args[index].as_str() {
            "--id" => {
                room_id = Some(
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
            arg if !arg.starts_with("--") && room_id.is_none() => {
                room_id = Some(arg.to_string());
                index += 1;
            }
            arg => bail!("unknown argument: {arg}"),
        }
    }

    let room_id = room_id.context("missing room ID")?;

    let runtime_context = CLI_RUNTIME_CONTEXT.get().unwrap();
    let workspace_namespace = hc_store::store::WorkspaceNamespace::new(
        runtime_context.tenant_id.as_deref().unwrap_or("local"),
        runtime_context.user_id.as_deref().unwrap_or("default"),
    );

    let repository = MemoryRoomRepository::with_namespace(workspace_root(), workspace_namespace);

    match repository.get_room_by_id(&room_id)? {
        Some(room) => {
            if json {
                let room_data = serde_json::json!({
                    "id": room.id,
                    "layer": format!("{:?}", room.layer).to_lowercase(),
                    "title": room.title,
                    "summary": room.summary,
                    "status": room.status,
                    "tags": room.tags,
                    "config": {
                        "execution_context": {
                            "working_directory": room.room_config.execution_context.working_directory,
                            "default_namespace": room.room_config.execution_context.default_namespace,
                            "environment": room.room_config.execution_context.environment,
                        }
                    },
                    "capabilities": room.inherited_capabilities.iter().map(|cap| {
                        serde_json::json!({
                            "id": cap.id,
                            "inheritance_type": format!("{:?}", cap.inheritance_type).to_lowercase()
                        })
                    }).collect::<Vec<_>>(),
                    "tools": room.inherited_tools.iter().map(|tool| {
                        serde_json::json!({
                            "id": tool.id,
                            "inheritance_type": format!("{:?}", tool.inheritance_type).to_lowercase()
                        })
                    }).collect::<Vec<_>>(),
                    "skills": room.inherited_skills.iter().map(|skill| {
                        serde_json::json!({
                            "id": skill.id,
                            "inheritance_type": format!("{:?}", skill.inheritance_type).to_lowercase()
                        })
                    }).collect::<Vec<_>>(),
                    "schedules": room.inherited_schedules.iter().map(|schedule| {
                        serde_json::json!({
                            "id": schedule.id,
                            "inheritance_type": format!("{:?}", schedule.inheritance_type).to_lowercase()
                        })
                    }).collect::<Vec<_>>(),
                });
                println!("{}", serde_json::to_string_pretty(&room_data)?);
            } else {
                println!("Memory Room: {}", room.id);
                println!("  Layer: {:?}", room.layer);
                println!("  Title: {}", room.title);
                if !room.summary.is_empty() {
                    println!("  Summary: {}", room.summary);
                }
                if !room.status.is_empty() {
                    println!("  Status: {}", room.status);
                }

                if !room.tags.is_empty() {
                    println!("  Tags: {}", room.tags.join(", "));
                }

                println!();
                println!("Execution Context:");
                if let Some(wd) = &room.room_config.execution_context.working_directory {
                    println!("  Working Directory: {}", wd);
                }
                if let Some(ns) = &room.room_config.execution_context.default_namespace {
                    println!("  Default Namespace: {}", ns);
                }
                if !room.room_config.execution_context.environment.is_empty() {
                    println!("  Environment Variables:");
                    for (key, value) in &room.room_config.execution_context.environment {
                        println!("    {}: {}", key, value);
                    }
                }

                println!();
                if !room.inherited_capabilities.is_empty() {
                    println!(
                        "Inherited Capabilities ({}):",
                        room.inherited_capabilities.len()
                    );
                    for cap in &room.inherited_capabilities {
                        println!("  - {} ({:?})", cap.id, cap.inheritance_type);
                    }
                    println!();
                }

                if !room.inherited_tools.is_empty() {
                    println!("Inherited Tools ({}):", room.inherited_tools.len());
                    for tool in &room.inherited_tools {
                        println!("  - {} ({:?})", tool.id, tool.inheritance_type);
                    }
                    println!();
                }

                if !room.inherited_skills.is_empty() {
                    println!("Inherited Skills ({}):", room.inherited_skills.len());
                    for skill in &room.inherited_skills {
                        println!("  - {} ({:?})", skill.id, skill.inheritance_type);
                    }
                    println!();
                }

                if !room.inherited_schedules.is_empty() {
                    println!("Inherited Schedules ({}):", room.inherited_schedules.len());
                    for schedule in &room.inherited_schedules {
                        println!("  - {} ({:?})", schedule.id, schedule.inheritance_type);
                    }
                }
            }
        }
        None => {
            if json {
                println!("{{\"error\": \"Room not found: {}\" }}", room_id);
            } else {
                println!("Room not found: {}", room_id);
            }
        }
    }

    Ok(())
}

pub(super) fn handle_room_capabilities(args: &[String]) -> Result<()> {
    let mut room_id = None;
    let mut json = false;
    let mut index = 0usize;

    while index < args.len() {
        match args[index].as_str() {
            "--id" => {
                room_id = Some(
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
            arg if !arg.starts_with("--") && room_id.is_none() => {
                room_id = Some(arg.to_string());
                index += 1;
            }
            arg => bail!("unknown argument: {arg}"),
        }
    }

    let room_id = room_id.context("missing room ID")?;

    let runtime_context = CLI_RUNTIME_CONTEXT.get().unwrap();
    let workspace_namespace = hc_store::store::WorkspaceNamespace::new(
        runtime_context.tenant_id.as_deref().unwrap_or("local"),
        runtime_context.user_id.as_deref().unwrap_or("default"),
    );

    let repository =
        MemoryRoomRepository::with_namespace(workspace_root(), workspace_namespace.clone());

    match repository.get_room_by_id(&room_id)? {
        Some(room) => {
            // 构建内存命名空间
            let memory_namespace = MemoryNamespace::new(
                runtime_context.tenant_id.as_deref().unwrap_or("local"),
                runtime_context.user_id.as_deref().unwrap_or("default"),
            );

            // 解析房间能力
            let resolver = RoomCapabilityResolver::new(memory_namespace);
            match resolver.resolve_room_capabilities(&room) {
                Ok(capabilities) => {
                    if json {
                        let capabilities_data = serde_json::json!({
                            "room_id": room_id,
                            "capabilities": capabilities.capabilities.iter().map(|cap| {
                                serde_json::json!({
                                    "id": cap.capability_ref.id,
                                    "inheritance_type": format!("{:?}", cap.capability_ref.inheritance_type).to_lowercase(),
                                    "resolved": true
                                })
                            }).collect::<Vec<_>>(),
                            "tools": capabilities.tools.iter().map(|tool| {
                                serde_json::json!({
                                    "id": tool.tool_ref.id,
                                    "inheritance_type": format!("{:?}", tool.tool_ref.inheritance_type).to_lowercase(),
                                    "resolved": true
                                })
                            }).collect::<Vec<_>>(),
                            "skills": capabilities.skills.iter().map(|skill| {
                                serde_json::json!({
                                    "id": skill.skill_ref.id,
                                    "inheritance_type": format!("{:?}", skill.skill_ref.inheritance_type).to_lowercase(),
                                    "resolved": true
                                })
                            }).collect::<Vec<_>>(),
                            "schedules": capabilities.schedules.iter().map(|schedule| {
                                serde_json::json!({
                                    "id": schedule.schedule_ref.id,
                                    "inheritance_type": format!("{:?}", schedule.schedule_ref.inheritance_type).to_lowercase(),
                                    "resolved": true
                                })
                            }).collect::<Vec<_>>(),
                        });
                        println!("{}", serde_json::to_string_pretty(&capabilities_data)?);
                    } else {
                        println!("Room Capabilities for: {}", room_id);
                        println!();

                        if !capabilities.capabilities.is_empty() {
                            println!(
                                "Resolved Capabilities ({}):",
                                capabilities.capabilities.len()
                            );
                            for cap in &capabilities.capabilities {
                                println!(
                                    "  - {} ({:?})",
                                    cap.capability_ref.id, cap.capability_ref.inheritance_type
                                );
                            }
                            println!();
                        }

                        if !capabilities.tools.is_empty() {
                            println!("Resolved Tools ({}):", capabilities.tools.len());
                            for tool in &capabilities.tools {
                                println!(
                                    "  - {} ({:?})",
                                    tool.tool_ref.id, tool.tool_ref.inheritance_type
                                );
                            }
                            println!();
                        }

                        if !capabilities.skills.is_empty() {
                            println!("Resolved Skills ({}):", capabilities.skills.len());
                            for skill in &capabilities.skills {
                                println!(
                                    "  - {} ({:?})",
                                    skill.skill_ref.id, skill.skill_ref.inheritance_type
                                );
                            }
                            println!();
                        }

                        if !capabilities.schedules.is_empty() {
                            println!("Resolved Schedules ({}):", capabilities.schedules.len());
                            for schedule in &capabilities.schedules {
                                println!(
                                    "  - {} ({:?})",
                                    schedule.schedule_ref.id,
                                    schedule.schedule_ref.inheritance_type
                                );
                            }
                            println!();
                        }

                        let total_count = capabilities.capabilities.len()
                            + capabilities.tools.len()
                            + capabilities.skills.len()
                            + capabilities.schedules.len();
                        if total_count == 0 {
                            println!("No resolved capabilities found for this room.");
                        } else {
                            println!("Total: {} resolved capabilities", total_count);
                        }
                    }
                }
                Err(err) => {
                    if json {
                        println!(
                            r#"{{"room_id": "{}", "error": "Failed to resolve capabilities: {}"}}"#,
                            room_id, err
                        );
                    } else {
                        println!(
                            "Failed to resolve capabilities for room {}: {}",
                            room_id, err
                        );
                    }
                }
            }
        }
        None => {
            if json {
                println!(r#"{{"error": "Room not found: {}"}}"#, room_id);
            } else {
                println!("Room not found: {}", room_id);
            }
        }
    }

    Ok(())
}

pub(super) fn handle_room_inherit(args: &[String]) -> Result<()> {
    let mut room_id = None;
    let mut capability_type = None;
    let mut capability_id = None;
    let mut inheritance_type = None;
    let mut json = false;
    let mut index = 0usize;

    while index < args.len() {
        match args[index].as_str() {
            "--room-id" => {
                room_id = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --room-id")?,
                );
                index += 2;
            }
            "--type" => {
                capability_type = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --type")?,
                );
                index += 2;
            }
            "--id" => {
                capability_id = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --id")?,
                );
                index += 2;
            }
            "--inheritance" => {
                inheritance_type = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --inheritance")?,
                );
                index += 2;
            }
            "--json" => {
                json = true;
                index += 1;
            }
            arg => bail!("unknown argument: {arg}"),
        }
    }

    let room_id = room_id.context("missing --room-id")?;
    let capability_type =
        capability_type.context("missing --type (capability, tool, skill, schedule)")?;
    let capability_id = capability_id.context("missing --id")?;
    let inheritance_type = inheritance_type.unwrap_or_else(|| "manual".to_string());

    let inheritance = match inheritance_type.as_str() {
        "manual" => InheritanceType::Manual,
        "auto" => InheritanceType::AutoDiscovered,
        "parent" => InheritanceType::FromParent,
        "sibling" => InheritanceType::FromSibling,
        "direct" => InheritanceType::Direct,
        _ => bail!(
            "invalid inheritance type: {} (must be manual, auto, parent, sibling, or direct)",
            inheritance_type
        ),
    };

    let workspace_namespace = runtime_namespace();
    let memory_namespace = MemoryNamespaceStorage::new(
        workspace_namespace.tenant_id.clone(),
        workspace_namespace.user_id.clone(),
    );
    let repository =
        MemoryRoomRepository::with_namespace(workspace_root(), workspace_namespace.clone());
    let resolver = RoomCapabilityResolver::new(memory_namespace);
    let mut room = repository
        .get_room_by_id(&room_id)?
        .with_context(|| format!("room not found: {room_id}"))?;

    match capability_type.as_str() {
        "capability" => resolver.add_capability_to_room(
            &mut room,
            CapabilityRef::new(&capability_id).with_inheritance_type(inheritance),
        )?,
        "tool" => resolver.add_tool_to_room(
            &mut room,
            ToolRef::new(&capability_id).with_inheritance_type(inheritance),
        )?,
        "skill" => resolver.add_skill_to_room(
            &mut room,
            SkillRef::new(&capability_id).with_inheritance_type(inheritance),
        )?,
        "schedule" => resolver.add_schedule_to_room(
            &mut room,
            ScheduleRef::new(&capability_id).with_inheritance_type(inheritance),
        )?,
        _ => bail!(
            "invalid type: {} (must be capability, tool, skill, or schedule)",
            capability_type
        ),
    }

    let path = repository.write_room(&room)?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "success": true,
                "room_id": room_id,
                "type": capability_type,
                "capability_id": capability_id,
                "inheritance": inheritance_type,
                "path": path,
            }))?
        );
    } else {
        println!(
            "Added {} inheritance of {} {} to room {} at {}",
            inheritance_type,
            capability_type,
            capability_id,
            room_id,
            path.display()
        );
    }

    Ok(())
}
