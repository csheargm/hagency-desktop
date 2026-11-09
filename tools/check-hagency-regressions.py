#!/usr/bin/env python3
"""Release checks for budget authority and chat regressions; no live account calls.

python3 tools/check-hagency-regressions.py --output .run/regressions
Includes the in-tree Client library HTTP loss/replay tests.
Server PostgreSQL integration tests must separately use a disposable database.
"""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
CLIENT = ROOT / "crates/hagency-client"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--cargo-config", type=Path,
                        help="Optional pinned framework config for the Desktop root checks")
    parser.add_argument("--start-at", help="Resume at this failed check; earlier recorded passes must be unaffected by the fix")
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()
    env.setdefault("RUST_MIN_STACK", "16777216")
    results = []
    previous = {}
    if args.start_at:
        previous = {row['check']: row for row in json.loads((output / 'report.json').read_text())}
    reached_start = not bool(args.start_at)

    def run(name, command, cwd=ROOT, rust_tests=False):
        nonlocal reached_start
        if not reached_start:
            reached_start = name == args.start_at
            if not reached_start:
                prior = previous.get(name)
                log = output / (name + '.log')
                if not prior or not prior['passed'] or not log.is_file():
                    raise SystemExit(f'Cannot reuse an unproven check: {name}')
                results.append(prior)
                print(name, 'REUSED', flush=True)
                return log.read_text()
        # Standalone tool/crate workspaces retain their own pinned lockfiles.
        # The native-accessibility candidate uses the reviewed framework patch
        # only for its integrating Desktop workspace.
        native_lock = None
        if args.cargo_config and cwd == ROOT and command[0] == "cargo" and "--manifest-path" not in command:
            # Cargo reports a lock update for overlapping Git/path patch-unused
            # entries even when resolving writes identical bytes. Keep the
            # dependency invariant explicit: offline resolution plus an exact
            # comparison of every resolved package (unused patch bookkeeping
            # is outside that graph), rather than waiving the gate.
            native_lock = (ROOT / 'Cargo.lock').read_bytes().split(b'[[patch.unused]]', 1)[0]
            command = [part for part in command if part != '--locked']
            if '--offline' not in command:
                command.insert(1, '--offline')
            command = [command[0], "--config", str(args.cargo_config.resolve()), *command[1:]]
        log = output / (name + ".log")
        with log.open("w") as stream:
            result = subprocess.run(command, cwd=cwd, env=env, stdout=stream, stderr=subprocess.STDOUT)
        text = log.read_text()
        count = sum(int(n) for n in re.findall(r"test result: ok\. (\d+) passed;", text))
        unchanged = native_lock is None or native_lock == (ROOT / 'Cargo.lock').read_bytes().split(b'[[patch.unused]]', 1)[0]
        passed = result.returncode == 0 and unchanged and (not rust_tests or count > 0)
        if not unchanged:
            with log.open('a') as stream:
                stream.write('\nNative framework check changed resolved Cargo.lock packages; reconcile and review before retrying.\n')
        results.append({"check": name, "passed": passed, "tests": count if rust_tests else None})
        (output / "report.json").write_text(json.dumps(results, indent=2) + "\n")
        print(name, "PASS" if passed else "FAIL", flush=True)
        if not passed:
            raise SystemExit(f"Inspect {log}; no release until this check passes")
        return text

    run("owner-source-pins", ["python3", "tools/check-owner-source-pins.py"])
    run("owner-source-boundary", ["python3", "tools/test-owner-source-pins.py"])
    run("package", ["bash", "tools/package-system-apps/check.sh"])
    run("script-vm", ["cargo", "build", "--locked", "--manifest-path", "tools/hagency-script-review/Cargo.toml"])
    vm = ROOT / "tools/hagency-script-review/target/debug/hagency-script-review"
    for name in ["check_full_bundle", "check_agent_budget", "check_agent_control", "check_pool_application", "check_entry_workspaces", "check_account_identity", "check_accounts", "check_owner_tasks", "check_encrypted_approvals", "check_pool_fixes", "check_shared_web", "check_settings_stability", "check_background_observations", "check_publication_first", "check_owner_dashboard_approvals", "check_interrupted_calls"]:
        run(name, ["python3", f"apps/hagency/tests/{name}.py", str(vm), "--output", str(output / name)])
    # Keep these together: a budget fix must not erase rendering, activity or answer actions.
    for name, pattern in [
        ("budget-host", "host::hagency::budget"),
        ("host-stack", "host::hagency::cache_tests::host_request_"),
        ("token-units", "token_unit_tests"),
        ("native-resource-units", "host::hagency::enrollment"),
        ("publication-host", "host::hagency::grants"),
        ("recurring-period-display", "host::hagency::pools::recurring_display_tests"),
        ("whitelist-accounts", "host::hagency::grant_people"),
        ("friend-search", "home::mobile::new_chat_tests"),
        ("space-management", "home::space_management::tests"),
        ("publication-ui", "grant_ui::"),
        ("resource-ceiling-ui", "resource_edit_ui::tests"),
        ("agent-chat", "agent_chat"),
        ("agent-question-cards", "hagency::pending_questions::tests"),
        ("shared-worker", "hagency::backend::shared_worker::tests"),
        ("chat-mermaid", "shared::chat_mermaid::tests"),
        ("link-preview", "link_preview"),
        ("linkify", "linkify"),
        ("activity", "shared_permission_notice"),
        ("theme", "theme::"),
        ("theme-contrast", "semantic_token_pairs_meet_wcag_aa"),
        ("chat-information", "home::mobile_chat_info"),
        ("agent-invitations", "hagency::backend::binding_recovery_tests"),
        ("modal-navigation", "app::back_navigation_tests"),
        ("upload-preview", "shared::file_upload_modal::tests"),
        ("compact-mentions", "shared::mention"),
        ("chat-information-layout", "desktop_information_columns_tests"),
        ("chat-layout", "home::main_desktop_ui::chat_layout_tests"),
        ("miniapp-dock", "mini_apps_side_tests"),
        ("owner-approval-navigation", "owner_approvals::tests"),
        ("owner-dashboard-host", "hagency::owner_dashboard::tests"),
        ("owner-hosting", "hagency::hosting::tests"),
        ("owner-daemon-handoff", "hagency::backend::retirement_tests"),
        ("owner-renewal-warnings", "renewal_warning_tests"),
        ("article-editor", "article_app::"),
    ]:
        run(name, ["cargo", "test", "--locked", "--lib", "--features", "onboarding-test-support", pattern], rust_tests=True)
    if sys.platform == "darwin":
        run("apple-chat-fonts", ["cargo", "test", "--locked", "--lib", "--features", "onboarding-test-support", "apple_fonts", "--", "--test-threads=1"], rust_tests=True)
        run("apple-system-fonts", ["cargo", "test", "--locked", "--manifest-path", "crates/article-makepad/Cargo.toml", "--lib", "apple_fonts", "--", "--test-threads=1"], rust_tests=True)
    run("answer-export", ["cargo", "test", "--locked", "--manifest-path", "crates/answer-export/Cargo.toml", "--lib"], rust_tests=True)
    # Separate workspace: Desktop's --lib gate does not execute the
    # owner-dashboard crate's own tests.
    run("owner-dashboard", ["cargo", "test", "--locked", "--manifest-path", "crates/owner-dashboard/Cargo.toml", "--lib"], rust_tests=True)
    run("native-question-protocol", ["cargo", "test", "--locked", "-p", "hagency", "--features", "shared-budget-execution,resource-adoption", "--lib", "agent_questions::tests"], cwd=CLIENT, rust_tests=True)
    run("native-postpaid-personal", ["cargo", "test", "--locked", "-p", "hagency-agent-local", "--all-targets", "postpaid"], cwd=CLIENT, rust_tests=True)
    run("native-personal-room-bridge", ["cargo", "test", "--locked", "-p", "hagency-agent-local", "--lib", "room_"], cwd=CLIENT, rust_tests=True)
    run("native-personal-claude-room", ["cargo", "test", "--locked", "-p", "hagency-agent-local", "--test", "runtime", "room::"], cwd=CLIENT, rust_tests=True)
    run("native-question-recovery", ["cargo", "test", "--locked", "-p", "hagency", "--features", "shared-budget-execution,resource-adoption", "--lib", "shared_execution::questions::", "--", "--test-threads=1"], cwd=CLIENT, rust_tests=True)
    run("native-question-recovery-auth", ["cargo", "test", "--locked", "-p", "hagency", "--features", "shared-budget-execution,resource-adoption", "--lib", "question_recovery_requires_authenticated_events_in_encrypted_rooms"], cwd=CLIENT, rust_tests=True)
    run("native-question-adapter", ["cargo", "test", "--locked", "-p", "hagency-runtime", "--features", "shared-context", "--all-targets", "user_input"], cwd=CLIENT, rust_tests=True)
    run("native-room-history", ["cargo", "test", "--locked", "-p", "hagency", "--features", "shared-budget-execution,resource-adoption", "--lib", "console::native::shared_execution::history::", "--", "--test-threads=1"], cwd=CLIENT, rust_tests=True)
    run("native-room-history-fields", ["cargo", "test", "--locked", "-p", "hagency", "--features", "shared-budget-execution,resource-adoption", "--lib", "console::owned_runtime::e2ee::history::tests", "--", "--test-threads=1"], cwd=CLIENT, rust_tests=True)
    run("native-key-wakeup", ["cargo", "test", "--locked", "-p", "hagency", "--features", "shared-budget-execution,resource-adoption", "--lib", "shared_work_sync_wakes", "--", "--test-threads=1"], cwd=CLIENT, rust_tests=True)
    run("native-room-history-protocol", ["cargo", "test", "--locked", "-p", "hagency-runtime", "--features", "shared-context", "--test", "session", "native_room_history", "--", "--test-threads=1"], cwd=CLIENT, rust_tests=True)
    run("native-server-management", ["cargo", "test", "--locked", "-p", "hagency", "--features", "shared-budget-execution,resource-adoption", "--lib", "server_management", "--", "--test-threads=1"], cwd=CLIENT, rust_tests=True)
    run("native-adopted-server-administration", ["cargo", "test", "--locked", "-p", "hagency", "--features", "shared-budget-execution,resource-adoption", "--lib", "server_administration", "--", "--test-threads=1"], cwd=CLIENT, rust_tests=True)
    run("native-server-journal", ["cargo", "test", "--locked", "-p", "hagency-store", "--features", "shared-budget-execution,resource-adoption", "--lib", "server_journal", "--", "--test-threads=1"], cwd=CLIENT, rust_tests=True)
    run("native-owner-daemon", ["cargo", "test", "--locked", "-p", "hagency", "--features", "shared-budget-execution,resource-adoption", "--lib", "daemon::", "--", "--test-threads=1"], cwd=CLIENT, rust_tests=True)
    run("native-owner-telemetry", ["cargo", "test", "--locked", "-p", "hagency", "--features", "shared-budget-execution,resource-adoption", "--lib", "console::native::telemetry::", "--", "--test-threads=1"], cwd=CLIENT, rust_tests=True)
    run("native-metering-ledger", ["cargo", "test", "--locked", "-p", "hagency-metering", "--test", "dashboard"], cwd=CLIENT, rust_tests=True)
    run("native-confirmation-deadlines", ["cargo", "test", "--locked", "-p", "hagency", "--features", "shared-budget-execution,resource-adoption", "--lib", "daemon::bridge::reviews::deadline_tests"], cwd=CLIENT, rust_tests=True)
    run("native-recurring-quota", ["cargo", "test", "--locked", "-p", "hagency-core", "--lib", "recurring_quota"], cwd=CLIENT, rust_tests=True)
    run("native-pool-wire", ["cargo", "test", "--locked", "-p", "hagency", "--features", "shared-budget-execution,resource-adoption", "--lib", "split_pool_limits_tests"], cwd=CLIENT, rust_tests=True)
    run("native-pool-publication", ["cargo", "test", "--locked", "-p", "hagency", "--features", "shared-budget-execution,resource-adoption", "--lib", "native_allocation_transport_", "--", "--test-threads=1"], cwd=CLIENT, rust_tests=True)
    run("native-recurring-transport", ["cargo", "test", "--locked", "-p", "hagency", "--features", "shared-budget-execution,resource-adoption", "--lib", "recurring_transport_tests"], cwd=CLIENT, rust_tests=True)
    run("native-room-workspaces", ["cargo", "test", "--locked", "-p", "hagency-runtime", "--features", "shared-context", "--lib", "room_workspace"], cwd=CLIENT, rust_tests=True)
    run("native-room-workspace-launch", ["cargo", "test", "--locked", "-p", "hagency", "--features", "shared-budget-execution", "--lib", "shared_tools_first_launch", "--", "--test-threads=1"], cwd=CLIENT, rust_tests=True)
    run("native-room-workspace-protocol", ["cargo", "test", "--locked", "-p", "hagency-runtime", "--features", "shared-context", "--test", "managed_qualification", "shared_room_workspace", "--", "--test-threads=1"], cwd=CLIENT, rust_tests=True)
    run("native-room-workspace-approvals", ["cargo", "test", "--locked", "-p", "hagency", "--features", "shared-budget-execution", "--lib", "shared_execution::approvals::tests", "--", "--test-threads=1"], cwd=CLIENT, rust_tests=True)
    for name, package, pattern in [
        ("native-agent-permissions", "hagency-store", "execution_policy"),
        ("native-agent-web-search", "hagency-store", "web_policy"),
        ("native-pool-policy-inheritance", "hagency-store", "pool_agent_inherits_owner_permissions_and_network"),
        ("native-pool-owner-renewal", "hagency", "pool_owner_renewal_policy"),
        ("native-permission-recovery", "hagency", "execution_policy"),
    ]:
        run(name, ["cargo", "test", "--locked", "-p", package, "--features", "shared-budget-execution,resource-adoption", "--lib", pattern, "--", "--test-threads=1"], cwd=CLIENT, rust_tests=True)
    run("native-owner-restart", ["cargo", "test", "--locked", "-p", "hagency", "--features", "shared-budget-execution", "--lib", "owner_host::tests", "--", "--test-threads=1"], cwd=CLIENT, rust_tests=True)
    for name, pattern in [("native-publication-form", "publication_form"), ("native-publication-recovery", "native_grant_publication_")]:
        run(name, ["cargo", "test", "--locked", "-p", "hagency", "--features", "shared-budget-execution", "--lib", pattern, "--", "--test-threads=1"], cwd=CLIENT, rust_tests=True)
    run("native-model-qualification", ["cargo", "test", "--locked", "-p", "hagency-core", "--test", "qualification"], cwd=CLIENT, rust_tests=True)
    run("native-model-catalog", ["cargo", "test", "--locked", "-p", "hagency", "--features", "shared-budget-execution", "--lib", "resource_onboarding::tests"], cwd=CLIENT, rust_tests=True)
    run("native-resource-creation", ["cargo", "test", "--locked", "-p", "hagency-store", "--features", "shared-budget-execution,resource-adoption", "--lib", "additional_configuration_", "--", "--test-threads=1"], cwd=CLIENT, rust_tests=True)
    probe_build = run("native-context-probe", ["cargo", "build", "--locked", "-p", "hagency-runtime", "--features", "shared-context", "--bin", "hagency-managed-runtime-probe", "--message-format=json"], cwd=CLIENT)
    artifacts = [json.loads(line) for line in probe_build.splitlines() if line.startswith("{")]
    probes = [item["executable"] for item in artifacts if item.get("reason") == "compiler-artifact" and item.get("target", {}).get("name") == "hagency-managed-runtime-probe" and item.get("executable")]
    if len(probes) != 1:
        raise SystemExit("Expected one freshly built offline context probe")
    env["HAGENCY_TEST_RUNTIME_PROBE"] = probes[0]
    run("native-postpaid-shared", ["cargo", "test", "--locked", "-p", "hagency-store", "--features", "shared-budget-execution,resource-adoption", "--lib", "postpaid_", "--", "--include-ignored", "--test-threads=1"], cwd=CLIENT, rust_tests=True)
    for name, package, pattern in [
        ("native-known-usage-recovery", "hagency-store", "recurring_"),
        ("native-never-started-store-recovery", "hagency-store", "shared_demand_v2_"),
        ("native-never-started-message-recovery", "hagency", "native_demand_never_started_attempt_retries_same_event_exactly_once"),
        ("native-demand-late-stop-recovery", "hagency", "native_demand_late_stop_and_logout_keep_unknown_state_and_deny_stale_offer"),
    ]:
        run(name, ["cargo", "test", "--locked", "-p", package, "--features", "shared-budget-execution,resource-adoption", "--lib", pattern, "--", "--include-ignored", "--test-threads=1"], cwd=CLIENT, rust_tests=True)
    run("native-question-execution", ["cargo", "test", "--locked", "-p", "hagency", "--features", "shared-budget-execution,resource-adoption", "--lib", "native_shared_question_", "--", "--include-ignored", "--test-threads=1"], cwd=CLIENT, rust_tests=True)
    run("native-room-history-input", ["cargo", "test", "--locked", "-p", "hagency-store", "--features", "shared-budget-execution", "--lib", "encrypted_shared_input_is_pinned", "--", "--ignored", "--test-threads=1"], cwd=CLIENT, rust_tests=True)
    run("native-thread-context", ["cargo", "test", "--locked", "-p", "hagency-store", "--features", "shared-budget-execution", "--lib", "shared_context_", "--", "--include-ignored", "--test-threads=1"], cwd=CLIENT, rust_tests=True)
    run("native-thread-context-http", ["cargo", "test", "--locked", "-p", "hagency", "--features", "shared-budget-execution", "--lib", "native_shared_context_", "--", "--include-ignored", "--test-threads=1"], cwd=CLIENT, rust_tests=True)
    run("native-first-turn-permissions", ["cargo", "test", "--locked", "-p", "hagency", "--features", "shared-budget-execution,resource-adoption", "--lib", "native_shared_owner_policy_applies_before_the_first_model_call", "--", "--ignored", "--test-threads=1"], cwd=CLIENT, rust_tests=True)
    if sys.platform == "darwin":
        run("native-macos-exit-signal", ["cargo", "test", "--locked", "-p", "hagency-platform", "--lib", "native_macos_signal_denial"], cwd=CLIENT, rust_tests=True)
    run("native-interrupted-calls", ["cargo", "test", "--locked", "-p", "hagency", "--features", "shared-budget-execution,resource-adoption", "--lib", "native_shared_interrupted_call", "--", "--include-ignored", "--test-threads=1"], cwd=CLIENT, rust_tests=True)
    run("native-admitted-call-cleanup", ["cargo", "test", "--locked", "-p", "hagency-store", "--features", "shared-budget-execution", "--lib", "shared_successor_call_failed_start", "--", "--include-ignored", "--test-threads=1"], cwd=CLIENT, rust_tests=True)
    run("native-codex-control", ["cargo", "test", "--locked", "-p", "hagency-runtime", "--features", "shared-context", "--test", "session", "native_codex_control", "--", "--test-threads=1"], cwd=CLIENT, rust_tests=True)
    run("native-encrypted-execution", ["cargo", "test", "--locked", "-p", "hagency", "--features", "shared-budget-execution", "--lib", "native_shared_encrypted_", "--", "--include-ignored", "--test-threads=1"], cwd=CLIENT, rust_tests=True)
    run("native-demand-promotion-recovery", ["cargo", "test", "--locked", "-p", "hagency", "--features", "shared-budget-execution", "--lib", "native_demand_recovered_promotion", "--", "--include-ignored", "--test-threads=1"], cwd=CLIENT, rust_tests=True)
    for name, pattern in [("native-budget", "agent_budget"), ("native-http-replay", "budget_http")]:
        run(name, ["cargo", "test", "--locked", "-p", "hagency", "--features", "shared-budget-execution", "--lib", pattern, "--", "--test-threads=1"], cwd=CLIENT, rust_tests=True)
    run("native-account-inventory", ["cargo", "test", "--locked", "-p", "hagency-store", "--features", "shared-budget-execution,resource-adoption", "--lib", "account_inventory", "--", "--test-threads=1"], cwd=CLIENT, rust_tests=True)
    run("native-owner-pool-limits", ["cargo", "test", "--locked", "-p", "hagency-store", "--features", "shared-budget-execution,resource-adoption", "--lib", "automatic_account_quota_does_not_couple_pools_or_erase_unknown_usage", "--", "--test-threads=1"], cwd=CLIENT, rust_tests=True)
    run("native-capacity", ["cargo", "test", "--locked", "-p", "hagency-store", "--features", "shared-budget-execution", "--lib", "pool_agent_topup_requires_owner_keeps_unknown_usage_and_conserves_siblings", "--", "--test-threads=1"], cwd=CLIENT, rust_tests=True)
    run("native-room-documents", ["cargo", "test", "--locked", "-p", "hagency", "--features", "shared-budget-execution,resource-adoption", "--lib", "documents::tests", "--", "--test-threads=1"], cwd=CLIENT, rust_tests=True)
    run("native-pdf-parser", ["cargo", "test", "--locked", "-p", "hagency", "--features", "shared-budget-execution,resource-adoption", "--test", "documents", "--", "--test-threads=1"], cwd=CLIENT, rust_tests=True)
    run("native-chat-file-protocol", ["cargo", "test", "--locked", "-p", "hagency-runtime", "--features", "shared-context", "--test", "session", "native_room_file", "--", "--test-threads=1"], cwd=CLIENT, rust_tests=True)
    run("native-chat-file-service", ["cargo", "test", "--locked", "-p", "hagency", "--features", "shared-budget-execution,resource-adoption", "--lib", "chat_files_", "--", "--test-threads=1"], cwd=CLIENT, rust_tests=True)
    run("native-chat-file-execution", ["cargo", "test", "--locked", "-p", "hagency", "--features", "shared-budget-execution,resource-adoption", "--lib", "native_shared_chat_files_cross_", "--", "--ignored", "--test-threads=1"], cwd=CLIENT, rust_tests=True)
    run("native-claude-room-tools", ["cargo", "test", "--locked", "-p", "hagency-runtime", "--test", "claude_session", "room_mcp", "--", "--test-threads=1"], cwd=CLIENT, rust_tests=True)
    run("native-postpaid-host", ["cargo", "test", "--locked", "-p", "hagency", "--features", "shared-budget-execution,resource-adoption", "--lib", "--", "--test-threads=1", "typed_http_postpaid_overrun", "typed_http_runtime_unreported_failure", "typed_http_runtime_budget_notice"], cwd=CLIENT, rust_tests=True)
    if not reached_start:
        raise SystemExit(f'Unknown start check: {args.start_at}')


if __name__ == "__main__":
    main()
