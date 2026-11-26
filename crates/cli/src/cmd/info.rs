use super::*;

pub(super) fn list_skills(
    config: &DcodeAiConfig,
    workspace_root: &Path,
    json: bool,
) -> anyhow::Result<()> {
    let skills = SkillCatalog::discover(workspace_root, &config.harness.skill_directories)
        .map_err(anyhow::Error::msg)?;
    if json {
        let output: Vec<_> = skills
            .into_iter()
            .map(|skill| {
                let source = skill.source_label().to_string();
                SkillOutput {
                    name: skill.name,
                    command: skill.command,
                    description: skill.description,
                    model: skill.model,
                    permission_mode: skill.permission_mode.map(|mode| format!("{mode:?}")),
                    context: format!("{:?}", skill.context),
                    source,
                    directory: skill.directory,
                }
            })
            .collect();
        print_json(&output, false)?;
    } else if skills.is_empty() {
        println!("No skills found");
    } else {
        for skill in skills {
            println!("{}", skill.summary_line());
        }
    }
    Ok(())
}

pub(super) fn handle_skills_add(
    source: &str,
    skill_filter: &[String],
    global: bool,
    workspace_root: &Path,
) -> anyhow::Result<()> {
    use dcode_ai_core::skill_installer::{install_skills, parse_source};

    let parsed = parse_source(source).map_err(anyhow::Error::msg)?;
    let installed = install_skills(&parsed, skill_filter, global, workspace_root)
        .map_err(anyhow::Error::msg)?;

    let scope = if global { "(global)" } else { "(local)" };
    println!(
        "Installed {} skill(s) {scope}: {}",
        installed.len(),
        installed.join(", ")
    );
    Ok(())
}

pub(super) fn handle_skills_remove(
    name: &str,
    global: bool,
    workspace_root: &Path,
) -> anyhow::Result<()> {
    use dcode_ai_core::skill_installer::remove_skill;

    remove_skill(name, global, workspace_root).map_err(anyhow::Error::msg)?;
    println!("Removed skill: {name}");
    Ok(())
}

pub(super) fn handle_skills_update(
    name: Option<&str>,
    workspace_root: &Path,
) -> anyhow::Result<()> {
    use dcode_ai_core::skill_installer::{
        SkillLock, SkillLockEntry, copy_skill_dir, discover_skills_in_dir, git_clone_to_temp,
        git_head_commit, lock_file_path, skills_dir,
    };

    let mut updated = 0u32;
    let mut up_to_date = 0u32;
    let mut skipped = 0u32;

    for global in [false, true] {
        let lock_path = lock_file_path(global, workspace_root);
        let mut lock = SkillLock::load(&lock_path).map_err(anyhow::Error::msg)?;
        let target = skills_dir(global, workspace_root);
        let mut changed = false;

        let entries: Vec<_> = lock
            .skills
            .iter()
            .filter(|(k, _)| name.is_none() || name == Some(k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        for (skill_name, entry) in entries {
            if entry.commit.is_none() {
                eprintln!("Skipping '{skill_name}' (installed from local path)");
                skipped += 1;
                continue;
            }

            let clone_url = entry
                .source
                .strip_prefix("github:")
                .map(|repo| format!("https://github.com/{repo}.git"));

            let Some(url) = clone_url else {
                eprintln!("Skipping '{skill_name}' (unknown source format)");
                skipped += 1;
                continue;
            };

            let tmp = git_clone_to_temp(&url).map_err(anyhow::Error::msg)?;
            let new_commit = git_head_commit(tmp.path()).ok();

            if new_commit.as_deref() == entry.commit.as_deref() {
                up_to_date += 1;
                continue;
            }

            let skills_path = if tmp.path().join("skills").is_dir() {
                tmp.path().join("skills")
            } else {
                tmp.path().to_path_buf()
            };

            let discovered = discover_skills_in_dir(&skills_path).map_err(anyhow::Error::msg)?;

            if let Some((_, src_dir)) = discovered.iter().find(|(n, _)| n == &skill_name) {
                let dest = target.join(&skill_name);
                copy_skill_dir(src_dir, &dest).map_err(anyhow::Error::msg)?;
                lock.upsert(
                    &skill_name,
                    SkillLockEntry {
                        source: entry.source.clone(),
                        commit: new_commit,
                        installed_at: chrono::Utc::now().to_rfc3339(),
                    },
                );
                changed = true;
                updated += 1;
            } else {
                eprintln!("Warning: skill '{skill_name}' no longer found in source repo");
                skipped += 1;
            }
        }

        if changed {
            lock.save(&lock_path).map_err(anyhow::Error::msg)?;
        }
    }

    println!("Updated {updated}, already up-to-date {up_to_date}, skipped {skipped}");
    Ok(())
}

pub(super) async fn show_memory(
    config: &DcodeAiConfig,
    workspace_root: &Path,
    json: bool,
) -> anyhow::Result<()> {
    let store = workspace_memory_store(config, workspace_root);
    let state = store.load().await.map_err(anyhow::Error::msg)?;
    if json {
        print_json(&state, false)?;
    } else if state.notes.is_empty() {
        println!("No memory notes stored");
    } else {
        for note in state.notes {
            println!(
                "{}  {}  {}",
                note.id,
                note.kind,
                note.title.unwrap_or_else(|| note.created_at.to_rfc3339())
            );
            println!("  {}", note.content.replace('\n', " "));
        }
    }
    Ok(())
}

pub(super) async fn add_memory_note(
    config: &DcodeAiConfig,
    workspace_root: &Path,
    kind: &str,
    text: &str,
    json: bool,
) -> anyhow::Result<()> {
    let store = workspace_memory_store(config, workspace_root);
    let note = MemoryNote {
        id: format!("{}-{}", kind, chrono::Utc::now().timestamp_millis()),
        created_at: chrono::Utc::now(),
        kind: kind.to_string(),
        title: None,
        content: text.trim().to_string(),
    };
    let state = store
        .append_note(note.clone(), config.memory.max_notes)
        .await
        .map_err(anyhow::Error::msg)?;
    if json {
        print_json(&note, false)?;
    } else {
        println!("Stored memory note {} ({})", note.id, kind);
        println!("Memory path: {}", store.path().display());
        println!("Total notes: {}", state.notes.len());
    }
    Ok(())
}

pub(super) async fn show_models(config: &DcodeAiConfig, json: bool) -> anyhow::Result<()> {
    let active_provider_models =
        dcode_ai_runtime::model_limits_api::fetch_provider_model_ids(config)
            .await
            .map_err(anyhow::Error::msg)?;
    let output = ModelCatalogOutput {
        default_provider: config.provider.default.display_name().to_string(),
        default_model: config.model.default_model.clone(),
        source: "provider_api",
        active_provider_models,
        provider_models: ProviderKind::ALL
            .into_iter()
            .map(|provider| ProviderModelOutput {
                provider: provider.display_name().to_string(),
                model: config.provider.model_for(provider).to_string(),
                base_url: config.provider.base_url_for(provider).to_string(),
                selected: provider == config.provider.default,
            })
            .collect(),
        aliases: config.model.aliases.clone(),
        thinking_enabled: config.model.enable_thinking,
        thinking_budget: config.model.thinking_budget,
    };
    if json {
        print_json(&output, false)?;
    } else {
        println!(
            "Default provider/model: {} / {}",
            output.default_provider, output.default_model
        );
        println!(
            "Thinking: {} (budget {})",
            if output.thinking_enabled { "on" } else { "off" },
            output.thinking_budget
        );
        println!("Provider models:");
        for provider in &output.provider_models {
            println!(
                "  {}{} -> {} ({})",
                provider.provider,
                if provider.selected { " [selected]" } else { "" },
                provider.model,
                provider.base_url
            );
        }
        println!("Live active-provider models:");
        for model in &output.active_provider_models {
            println!("  {model}");
        }
        for (alias, target) in output.aliases {
            println!("  {alias} -> {target}");
        }
    }
    Ok(())
}

/// Live reachability probe for one provider: fetch its model catalog with its
/// own credentials.
pub(super) async fn probe_provider_health(config: &DcodeAiConfig, kind: ProviderKind) -> String {
    let signed_in = match kind {
        ProviderKind::Anthropic => {
            dcode_ai_common::provider_runtime::anthropic_signed_in(&config.provider.anthropic)
        }
        ProviderKind::Copilot => {
            config.provider.api_key_present_for(kind)
                || AuthStore::load().unwrap_or_default().copilot.is_some()
        }
        ProviderKind::Local => dcode_ai_core::provider::local::local_server_running().await,
    };
    if !signed_in {
        return "skipped (not signed in)".to_string();
    }
    let mut cfg = config.clone();
    cfg.set_default_provider(kind);
    match tokio::time::timeout(
        std::time::Duration::from_secs(10),
        dcode_ai_runtime::model_limits_api::fetch_provider_model_ids(&cfg),
    )
    .await
    {
        Ok(Ok(ids)) => format!("ok ({} models)", ids.len()),
        Ok(Err(err)) => format!("FAIL: {err}"),
        Err(_) => "FAIL: timed out (10s)".to_string(),
    }
}

pub(super) async fn show_doctor(
    config: &DcodeAiConfig,
    workspace_root: &Path,
    json: bool,
    check: bool,
) -> anyhow::Result<()> {
    let skills = SkillCatalog::discover(workspace_root, &config.harness.skill_directories)
        .map(|skills| skills.len())
        .unwrap_or(0);
    let binary_path = std::env::current_exe().ok();
    let runtime_socket_dir = runtime_socket_dir();

    // Optional live probes (network); off by default so doctor stays instant.
    let mut health: Vec<ProviderHealth> = Vec::new();
    if check {
        for kind in ProviderKind::ALL {
            health.push(ProviderHealth {
                provider: kind.display_name().to_string(),
                status: probe_provider_health(config, kind).await,
            });
        }
    }
    let output = DoctorOutput {
        binary_name: env!("CARGO_PKG_NAME").to_string(),
        binary_version: env!("CARGO_PKG_VERSION").to_string(),
        binary_path,
        platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        runtime_socket_dir,
        checksum_tool: detect_checksum_tool().map(str::to_string),
        provider: config.provider.default.display_name().to_string(),
        default_model: config.model.default_model.clone(),
        providers: ProviderKind::ALL
            .into_iter()
            .map(|provider| ProviderDoctorStatus {
                provider: provider.display_name().to_string(),
                selected: provider == config.provider.default,
                api_key_present: config.provider.api_key_present_for(provider),
                api_key_env: config.provider.api_key_env_for(provider).to_string(),
                model: config.provider.model_for(provider).to_string(),
                base_url: config.provider.base_url_for(provider).to_string(),
            })
            .collect(),
        provider_health: health,
        mcp_server_count: config
            .mcp
            .servers
            .iter()
            .filter(|server| server.enabled)
            .count(),
        skill_count: skills,
        memory_path: if config.memory.file_path.is_absolute() {
            config.memory.file_path.clone()
        } else {
            workspace_root.join(&config.memory.file_path)
        },
    };
    if json {
        print_json(&output, false)?;
    } else {
        println!("Binary: {} {}", output.binary_name, output.binary_version);
        if let Some(path) = &output.binary_path {
            println!("Binary path: {}", path.display());
        }
        println!("Platform: {}", output.platform);
        println!(
            "Runtime socket dir: {}",
            output.runtime_socket_dir.display()
        );
        println!(
            "Checksum tool: {}",
            output.checksum_tool.as_deref().unwrap_or("missing")
        );
        println!("Provider: {}", output.provider);
        println!("Default model: {}", output.default_model);
        println!("Providers (sign-in is OAuth: `dcode-ai login anthropic|copilot`):");
        let store = AuthStore::load().unwrap_or_default();
        for provider in &output.providers {
            let signed_in = match provider.provider.as_str() {
                "Claude" => {
                    store.anthropic.is_some()
                        || anthropic_signed_in(&config.provider.anthropic)
                        || std::env::var_os("ANTHROPIC_AUTH_TOKEN").is_some()
                }
                _ => store.copilot.is_some() || provider.api_key_present,
            };
            println!(
                "  {}{}: {} · model {} · {}",
                provider.provider,
                if provider.selected { " [selected]" } else { "" },
                if signed_in {
                    "signed in"
                } else {
                    "not signed in"
                },
                provider.model,
                provider.base_url
            );
        }
        if !output.provider_health.is_empty() {
            println!("Provider health (--check):");
            for entry in &output.provider_health {
                println!("  {}: {}", entry.provider, entry.status);
            }
        } else if !check {
            println!("Provider health: run `dcode-ai doctor --check` for live probes");
        }
        let signins = dcode_ai_cli::signin::check(config, true).await;
        if !signins.is_empty() {
            println!("Sign-ins:");
            let now = chrono::Utc::now();
            for s in &signins {
                println!("  {}", dcode_ai_cli::signin::doctor_line(s, now));
                if let Some(w) = dcode_ai_cli::signin::warning(s, now) {
                    println!("    {w}");
                }
            }
        }
        println!("Skills discovered: {}", output.skill_count);
        println!("MCP servers enabled: {}", output.mcp_server_count);
        println!("Memory path: {}", output.memory_path.display());
    }
    Ok(())
}

pub(super) fn runtime_socket_dir() -> PathBuf {
    dcode_ai_runtime::ipc::runtime_ipc_dir()
}

pub(super) fn detect_checksum_tool() -> Option<&'static str> {
    if command_exists("sha256sum") {
        Some("sha256sum")
    } else if command_exists("shasum") {
        Some("shasum")
    } else {
        None
    }
}

pub(super) fn command_exists(command: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(command).is_file()))
}

pub(super) fn show_config(
    config: &DcodeAiConfig,
    workspace_root: &Path,
    json: bool,
) -> anyhow::Result<()> {
    if json {
        print_json(config, false)?;
    } else {
        println!(
            "Global config: {}",
            dcode_ai_common::config::global_config_path()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "<unavailable>".into())
        );
        println!(
            "Workspace config: {}",
            dcode_ai_common::config::workspace_config_path(workspace_root).display()
        );
        println!("Default provider: {:?}", config.provider.default);
        println!("Default model: {}", config.model.default_model);
        println!("Permission mode: {:?}", config.permissions.mode);
        println!("Provider endpoints:");
        for provider in ProviderKind::ALL {
            println!(
                "  {} -> model={} base_url={}",
                provider.display_name(),
                config.provider.model_for(provider),
                config.provider.base_url_for(provider)
            );
        }
        println!(
            "Memory path: {}",
            workspace_memory_store(config, workspace_root)
                .path()
                .display()
        );
        println!("Skill directories:");
        for path in &config.harness.skill_directories {
            let resolved = if path.is_absolute() {
                path.clone()
            } else {
                workspace_root.join(path)
            };
            println!("  {}", resolved.display());
        }
    }
    Ok(())
}

pub(super) fn generate_shell_completion(shell: ClapShell) {
    let mut cmd = Cli::command();
    let bin_name = "dcode-ai";

    match shell {
        ClapShell::Bash => {
            generate(
                clap_complete::shells::Bash,
                &mut cmd,
                bin_name,
                &mut std::io::stdout(),
            );
        }
        ClapShell::Zsh => {
            generate(
                clap_complete::shells::Zsh,
                &mut cmd,
                bin_name,
                &mut std::io::stdout(),
            );
        }
        ClapShell::Fish => {
            generate(
                clap_complete::shells::Fish,
                &mut cmd,
                bin_name,
                &mut std::io::stdout(),
            );
        }
        ClapShell::PowerShell => {
            generate(
                clap_complete::shells::PowerShell,
                &mut cmd,
                bin_name,
                &mut std::io::stdout(),
            );
        }
        ClapShell::Elvish => {
            generate(
                clap_complete::shells::Elvish,
                &mut cmd,
                bin_name,
                &mut std::io::stdout(),
            );
        }
    }
}
