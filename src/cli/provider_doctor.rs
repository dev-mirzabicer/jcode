//! `jcode provider-doctor` command: a user-facing strict provider/model diagnostic.

use std::io::IsTerminal;

use anyhow::{Context, Result, anyhow};

use crate::live_tests::LiveVerificationStageStatus;
use jcode_provider_doctor::claude_boundaries::{
    CLAUDE_OAUTH_BOUNDARY_CONTRACT, run_claude_oauth_boundaries,
};
use jcode_provider_doctor::claude_contract::{
    CLAUDE_OAUTH_CONTRACT, capture_claude_sse_fixtures, run_claude_oauth_contract,
};
use jcode_provider_doctor::{
    DoctorReport, DoctorTier, NativeProviderKind, native_doctor_supports_provider,
    run_antigravity_native_e2e, run_claude_native_e2e, run_generic_native_e2e, run_provider_e2e,
};

pub async fn run_provider_doctor_command(
    provider: &str,
    model: Option<&str>,
    tier: &str,
    emit_json: bool,
) -> Result<()> {
    let tier: DoctorTier = tier
        .parse()
        .map_err(|message: String| anyhow!("{message}"))?;

    // Native-runtime providers cannot be driven by the OpenAI-compatible doctor;
    // route them to their native drivers, which exercise the production runtime.
    // Claude and Antigravity keep bespoke drivers (unusual credential/catalog
    // stories); everything else flows through the generic native driver.
    if native_doctor_supports_provider(provider) {
        let normalized = crate::auth::lifecycle::normalized_auth_provider_id(Some(provider));
        let report = match normalized {
            Some("claude") => run_claude_native_e2e(provider, model, tier).await?,
            Some("antigravity") => run_antigravity_native_e2e(provider, model, tier).await?,
            Some(other) => {
                let kind = NativeProviderKind::from_normalized(other)
                    .ok_or_else(|| anyhow!("`{provider}` has no native provider-doctor driver"))?;
                run_generic_native_e2e(kind, model, tier).await?
            }
            None => anyhow::bail!("`{provider}` has no native provider-doctor driver"),
        };
        emit_report(&report, emit_json);
        return if report.tier_passed {
            Ok(())
        } else {
            anyhow::bail!("provider-doctor: one or more checks failed for {provider}")
        };
    }

    let profile =
        crate::provider_catalog::openai_compatible_profile_by_id(provider).with_context(|| {
            format!(
                "`{provider}` is not a known OpenAI-compatible provider. \
                 Run `jcode provider-test-coverage` to see provider ids, or check your spelling."
            )
        })?;
    let resolved = crate::provider_catalog::resolve_openai_compatible_profile(profile);

    // Resolve the API key when the tier needs one.
    let api_key = if tier.requires_api_key() {
        let key = crate::provider_catalog::load_api_key_from_env_or_config(
            &resolved.api_key_env,
            &resolved.env_file,
        )
        .with_context(|| {
            format!(
                "no API key found for `{provider}` (looked in env `{}` and `{}`). \
                 Run `jcode login --provider {provider}`, or use `--tier offline` to check wiring only.",
                resolved.api_key_env, resolved.env_file
            )
        })?;
        Some(key)
    } else {
        None
    };

    let report = run_provider_e2e(profile, api_key.as_deref(), model, tier).await?;

    emit_report(&report, emit_json);

    // Non-zero exit when the chosen tier did not fully pass, so scripts/CI can gate on it.
    if report.tier_passed {
        Ok(())
    } else {
        anyhow::bail!("provider-doctor: one or more checks failed for {provider}")
    }
}

/// `jcode provider-doctor <provider> --contract <name>`: a live provider-contract
/// probe that writes a redacted JSON report and fails when an outcome drifts
/// from the recorded observation.
pub async fn run_provider_contract_command(
    provider: &str,
    contract: &str,
    model: Option<&str>,
    out: Option<std::path::PathBuf>,
    capture_sse: Option<std::path::PathBuf>,
) -> Result<()> {
    use crate::provider::Provider;
    use jcode_provider_anthropic_runtime::AnthropicProvider;

    anyhow::ensure!(
        contract == CLAUDE_OAUTH_CONTRACT || contract == CLAUDE_OAUTH_BOUNDARY_CONTRACT,
        "unknown provider contract `{contract}`; supported: {CLAUDE_OAUTH_CONTRACT}, {CLAUDE_OAUTH_BOUNDARY_CONTRACT}"
    );
    anyhow::ensure!(
        crate::auth::lifecycle::normalized_auth_provider_id(Some(provider)) == Some("claude"),
        "the `{contract}` contract runs against the `claude` provider"
    );
    let model = model.context("--model is required for a provider contract")?;
    let runtime = AnthropicProvider::new();
    runtime
        .pin_credential_mode_for_doctor(true)
        .context("Claude OAuth credential unavailable; run `jcode login --provider claude`")?;
    runtime.set_model(model)?;

    let registry =
        crate::tool::Registry::builtin_tool_surface(std::sync::Arc::new(AnthropicProvider::new()))
            .await;
    let tools = registry.definitions(None).await;
    if let Some(dir) = capture_sse {
        for path in capture_claude_sse_fixtures(&runtime, &tools, &dir).await? {
            println!("Captured {}", path.display());
        }
        return Ok(());
    }
    let report = if contract == CLAUDE_OAUTH_BOUNDARY_CONTRACT {
        run_claude_oauth_boundaries(&runtime, &tools).await?
    } else {
        run_claude_oauth_contract(&runtime, &tools).await?
    };

    let path = match out {
        Some(path) => path,
        None => crate::storage::jcode_dir()?
            .join("provider-contract")
            .join(format!(
                "{contract}-{model}-{}.json",
                chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
            )),
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(&report)?)
        .with_context(|| format!("writing {}", path.display()))?;

    println!(
        "Provider contract {} on {} ({} tools), {}",
        report.contract, report.model, report.tool_count, report.date
    );
    for probe in &report.probes {
        let verdict = match probe.agrees_with_gate0 {
            Some(true) => "agrees",
            Some(false) => "DRIFT",
            None => "recorded",
        };
        println!(
            "  [{:<8}] {:<29} {} observed={:?} gate0={:?}\n             {}",
            verdict, probe.id, probe.status, probe.observed, probe.gate0, probe.summary
        );
    }
    for note in &report.notes {
        println!("  note: {note}");
    }
    println!("Report: {}", path.display());
    let drift = report.drift();
    anyhow::ensure!(
        drift.is_empty(),
        "provider contract drifted from Gate 0 at: {}",
        drift.iter().map(|p| p.id).collect::<Vec<_>>().join(", ")
    );
    Ok(())
}

fn emit_report(report: &DoctorReport, emit_json: bool) {
    if emit_json {
        println!("{}", report_to_json(report));
    } else {
        let colorize = std::io::stdout().is_terminal()
            && std::env::var_os("NO_COLOR").is_none()
            && std::env::var_os("JCODE_NO_COLOR").is_none();
        print!("{}", format_report(report, colorize));
    }
}

fn status_symbol(status: LiveVerificationStageStatus) -> &'static str {
    match status {
        LiveVerificationStageStatus::Passed => "PASS",
        LiveVerificationStageStatus::Failed => "FAIL",
        LiveVerificationStageStatus::Blocked => "BLOCK",
        LiveVerificationStageStatus::Skipped => "skip",
        LiveVerificationStageStatus::NotRun => "----",
    }
}

fn status_color(status: LiveVerificationStageStatus) -> &'static str {
    match status {
        LiveVerificationStageStatus::Passed => "32", // green
        LiveVerificationStageStatus::Failed | LiveVerificationStageStatus::Blocked => "31", // red
        LiveVerificationStageStatus::Skipped => "90", // dim
        LiveVerificationStageStatus::NotRun => "90",
    }
}

fn format_report(report: &DoctorReport, colorize: bool) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Provider doctor: {} / {}\n",
        report.provider_label,
        if report.model.is_empty() {
            "<no model>"
        } else {
            &report.model
        }
    ));
    out.push_str(&format!("Tier: {} ", report.tier.as_str()));
    out.push_str(match report.tier {
        DoctorTier::Offline => "(no API key, no spend: validates jcode wiring only)\n",
        DoctorTier::Catalog => "(API key, ~no spend: adds live catalog fetch)\n",
        DoctorTier::Full => "(API key, spends balance: chat + streaming + tools)\n",
    });
    out.push_str(
        "Each line is one strict checkpoint. PASS/FAIL exercise it; skip means the\n\
         current tier does not run it (use --tier full for the API-dependent ones).\n\n",
    );

    for check in &report.checks {
        let symbol = status_symbol(check.status);
        let line = format!("  [{symbol:>5}] {:<38} {}\n", check.label, check.detail);
        if colorize {
            let color = status_color(check.status);
            out.push_str(&format!("\x1b[{color}m{line}\x1b[0m"));
        } else {
            out.push_str(&line);
        }
    }

    out.push('\n');
    if report.tier.spends_balance() {
        out.push_str(&format!(
            "Spend this run: {}\n",
            report.spend.human_summary()
        ));
    }
    if report.strict_passed {
        out.push_str("Verdict: READY. Every strict checkpoint passed for this provider/model.\n");
    } else if report.tier_passed {
        out.push_str(&format!(
            "Verdict: tier `{}` passed. Run `--tier full` to confirm full readiness (spends balance).\n",
            report.tier.as_str()
        ));
    } else if let Some(failure) = report.first_failure() {
        out.push_str(&format!(
            "Verdict: FAILED at `{}`.\n  Reason: {}\n",
            failure.label, failure.detail
        ));
        out.push_str(&next_step_hint(failure.checkpoint));
    } else {
        out.push_str("Verdict: FAILED.\n");
    }
    out
}

fn next_step_hint(checkpoint: &str) -> String {
    use crate::live_tests::checkpoints as cp;
    let hint = match checkpoint {
        cp::AUTH_CREDENTIAL_LOADED => {
            "  Next: run `jcode login --provider <provider>` to store a working credential."
        }
        cp::MODEL_CATALOG_LIVE_ENDPOINT => {
            "  Next: the live /models call failed. Check the key, network, and provider status."
        }
        cp::CATALOG_HOT_RELOAD_CURRENT_SESSION
        | cp::PICKER_LIVE_MODELS
        | cp::PICKER_FALLBACK_LABELING
        | cp::MODEL_SWITCH_ROUTE => {
            "  Next: this is a jcode-side routing/picker bug for this provider. \
             Please file an issue with this output."
        }
        cp::NON_STREAMING_CHAT_COMPLETION | cp::STREAMING_CHAT_COMPLETION => {
            "  Next: the model did not return a usable completion. Try another model from the catalog."
        }
        cp::TOOL_CALL_PARSE
        | cp::TOOL_EXECUTION_LOOP
        | cp::TOOL_RESULT_FOLLOWUP
        | cp::REAL_JCODE_TOOL_SMOKE => {
            "  Next: this model did not produce a valid tool call. It may not support tools well."
        }
        _ => "",
    };
    if hint.is_empty() {
        String::new()
    } else {
        format!("{hint}\n")
    }
}

fn report_to_json(report: &DoctorReport) -> String {
    let checks: Vec<serde_json::Value> = report
        .checks
        .iter()
        .map(|check| {
            serde_json::json!({
                "checkpoint": check.checkpoint,
                "label": check.label,
                "status": status_symbol(check.status).to_ascii_lowercase(),
                "detail": check.detail,
            })
        })
        .collect();
    serde_json::to_string_pretty(&serde_json::json!({
        "provider_id": report.provider_id,
        "provider_label": report.provider_label,
        "model": report.model,
        "tier": report.tier.as_str(),
        "tier_passed": report.tier_passed,
        "strict_passed": report.strict_passed,
        "spend": report.spend.to_json(),
        "checks": checks,
    }))
    .unwrap_or_else(|_| "{}".to_string())
}
