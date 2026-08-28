use crate::fixture::FixtureFile;
use crate::transport::Transport;
use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

/// Wraps any [`Transport`] and captures every response so the run can be
/// turned into a replayable fixture with [`RecordingTransport::into_fixture`].
///
/// Wired for tests today; a CLI flag to record a real run is the follow-up
/// that makes this the production record path.
#[cfg_attr(not(test), allow(dead_code))]
pub struct RecordingTransport<T: Transport> {
    inner: T,
    entries: HashMap<String, serde_json::Value>,
}

impl<T: Transport> RecordingTransport<T> {
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn new(inner: T) -> Self {
        RecordingTransport {
            inner,
            entries: HashMap::new(),
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn into_fixture(self) -> FixtureFile {
        FixtureFile {
            fixture_version: crate::fixture::FIXTURE_VERSION,
            entries: self.entries,
        }
    }
}
impl<T: Transport> Transport for RecordingTransport<T> {
    fn deploy_contract(
        &mut self,
        wasm_path: &Path,
        source: &str,
        network: &str,
        package_name: &str,
    ) -> Result<String> {
        let result = self
            .inner
            .deploy_contract(wasm_path, source, network, package_name)?;
        let key = format!("deploy:{}", package_name);
        self.entries.insert(key, Value::String(result.clone()));
        Ok(result)
    }

    fn build_invoke_xdr(
        &mut self,
        contract_id: &str,
        source: &str,
        network: &str,
        function: &str,
        func_args: &[String],
        package: &str,
    ) -> Result<String> {
        let result = self.inner.build_invoke_xdr(
            contract_id,
            source,
            network,
            function,
            func_args,
            package,
        )?;
        let key = format!("invoke:{}:{}", package, function);
        self.entries.insert(key, Value::String(result.clone()));
        Ok(result)
    }

    fn simulate_transaction(
        &mut self,
        b64_xdr: &str,
        package: &str,
        function: &str,
    ) -> Result<Value> {
        let result = self
            .inner
            .simulate_transaction(b64_xdr, package, function)?;
        let key = format!("simulate:{}:{}", package, function);
        self.entries.insert(key, result.clone());
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replay::ReplayTransport;
    use serde_json::json;

    /// Deterministic in-memory transport standing in for the network.
    struct MockTransport;

    impl Transport for MockTransport {
        fn deploy_contract(
            &mut self,
            _wasm_path: &Path,
            _source: &str,
            _network: &str,
            package_name: &str,
        ) -> anyhow::Result<String> {
            Ok(format!("C{}", package_name))
        }

        fn build_invoke_xdr(
            &mut self,
            _contract_id: &str,
            _source: &str,
            _network: &str,
            function: &str,
            _func_args: &[String],
            _package: &str,
        ) -> anyhow::Result<String> {
            Ok(format!("XDR:{}", function))
        }

        fn simulate_transaction(
            &mut self,
            _b64_xdr: &str,
            _package: &str,
            function: &str,
        ) -> anyhow::Result<Value> {
            Ok(json!({ "result": { "ok": true, "fn": function } }))
        }
    }

    /// Record a run, persist the fixture, load it back, and replay it: every
    /// response must come back identical. This is the property that lets the
    /// crate be tested without touching testnet.
    #[test]
    fn record_then_replay_round_trip_preserves_responses() {
        let mut recording = RecordingTransport::new(MockTransport);
        let deploy_id = recording
            .deploy_contract(Path::new("c.wasm"), "alice", "testnet", "pkg")
            .unwrap();
        let xdr = recording
            .build_invoke_xdr("C1", "alice", "testnet", "do_work", &[], "pkg")
            .unwrap();
        let sim = recording
            .simulate_transaction(&xdr, "pkg", "do_work")
            .unwrap();

        let fixture = recording.into_fixture();

        // Serialize → load → replay.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        fixture.save(tmp.path()).unwrap();
        let loaded = FixtureFile::load(tmp.path()).unwrap();

        let mut replay = ReplayTransport::new(loaded);
        assert_eq!(
            replay
                .deploy_contract(Path::new("c.wasm"), "alice", "testnet", "pkg")
                .unwrap(),
            deploy_id
        );
        assert_eq!(
            replay
                .build_invoke_xdr("C1", "alice", "testnet", "do_work", &[], "pkg")
                .unwrap(),
            xdr
        );
        assert_eq!(
            replay.simulate_transaction(&xdr, "pkg", "do_work").unwrap(),
            sim
        );
    }

    #[test]
    fn replay_reports_missing_entry() {
        let mut replay = ReplayTransport::new(FixtureFile::new());
        let err = replay
            .simulate_transaction("xdr", "pkg", "missing")
            .unwrap_err();
        assert!(err.to_string().contains("Fixture not found"));
    }

    #[test]
    fn fixture_rejects_wrong_version() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            serde_json::to_string(&json!({
                "fixture_version": 999,
                "entries": {}
            }))
            .unwrap(),
        )
        .unwrap();
        let err = FixtureFile::load(tmp.path()).unwrap_err();
        assert!(err.to_string().contains("version mismatch"));
    }

    // ── Malformed / truncated recording tests ───────────────────────────

    /// An empty file is not a valid fixture; load must fail with a
    /// parse error, not a panic or partial result.
    #[test]
    fn fixture_rejects_empty_file() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "").unwrap();
        let err = FixtureFile::load(tmp.path()).unwrap_err();
        // serde_json reports EOF or unexpected end of input
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("eof") || msg.contains("end of input") || msg.contains("parse"),
            "expected parse/EOF error, got: {msg}"
        );
    }

    /// Arbitrary non-JSON bytes produce a clear parse error.
    #[test]
    fn fixture_rejects_garbage_content() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), b"this is not json at all").unwrap();
        let err = FixtureFile::load(tmp.path()).unwrap_err();
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("parse") || msg.contains("json"),
            "expected JSON parse error, got: {msg}"
        );
    }

    /// Valid JSON that is missing the `fixture_version` field must fail
    /// at deserialization, not produce a default version.
    #[test]
    fn fixture_rejects_missing_version_field() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            serde_json::to_string(&json!({ "entries": {} })).unwrap(),
        )
        .unwrap();
        let err = FixtureFile::load(tmp.path()).unwrap_err();
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("parse") || msg.contains("deserialize") || msg.contains("missing"),
            "expected deserialization error, got: {msg}"
        );
    }

    /// Valid JSON that is missing the `entries` field must fail.
    #[test]
    fn fixture_rejects_missing_entries_field() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            serde_json::to_string(&json!({ "fixture_version": 1 })).unwrap(),
        )
        .unwrap();
        let err = FixtureFile::load(tmp.path()).unwrap_err();
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("parse") || msg.contains("deserialize") || msg.contains("missing"),
            "expected deserialization error, got: {msg}"
        );
    }

    /// A JSON string cut off mid-stream (truncated) must fail with a
    /// parse error, not return a partially populated fixture.
    #[test]
    fn fixture_rejects_truncated_json() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        // Write the start of a valid fixture then chop it off.
        let full = r#"{"fixture_version":1,"entries":{"deploy:pkg":"val"}}"#;
        let truncated_len = full.len() * 2 / 3;
        std::fs::write(tmp.path(), &full[..truncated_len]).unwrap();
        let err = FixtureFile::load(tmp.path()).unwrap_err();
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("parse") || msg.contains("eof") || msg.contains("end of input"),
            "expected parse/EOF error for truncated JSON, got: {msg}"
        );
    }

    // ── Replay missing-entry tests ─────────────────────────────────────

    /// `deploy_contract` on an empty fixture returns a clear error.
    #[test]
    fn replay_reports_missing_deploy_entry() {
        let mut replay = ReplayTransport::new(FixtureFile::new());
        let err = replay
            .deploy_contract(Path::new("c.wasm"), "alice", "testnet", "pkg")
            .unwrap_err();
        assert!(err.to_string().contains("Fixture not found"));
    }

    /// `build_invoke_xdr` on an empty fixture returns a clear error.
    #[test]
    fn replay_reports_missing_invoke_entry() {
        let mut replay = ReplayTransport::new(FixtureFile::new());
        let err = replay
            .build_invoke_xdr("C1", "alice", "testnet", "do_work", &[], "pkg")
            .unwrap_err();
        assert!(err.to_string().contains("Fixture not found"));
    }

    // ── Cross-network behaviour ─────────────────────────────────────────

    /// ReplayTransport silently ignores the `network` parameter: a
    /// fixture recorded against `testnet` replays without complaint even
    /// when the caller asks for `futurenet`.  This is a known gap — the
    /// recording format does not embed or validate the network, so stale
    /// fixtures can produce plausible-looking numbers from the wrong
    /// network.  Pinning this behaviour here so a follow-up can close it.
    #[test]
    fn replay_silently_ignores_network_mismatch() {
        let mut recording = RecordingTransport::new(MockTransport);
        // Record against testnet.
        let deploy_id = recording
            .deploy_contract(Path::new("c.wasm"), "alice", "testnet", "pkg")
            .unwrap();
        let fixture = recording.into_fixture();
        let mut replay = ReplayTransport::new(fixture);
        // Replay against futurenet — the network arg is ignored.
        let result = replay
            .deploy_contract(Path::new("c.wasm"), "alice", "futurenet", "pkg")
            .unwrap();
        assert_eq!(result, deploy_id);
    }

    // ── Multi-entry round-trip ──────────────────────────────────────────

    /// Record multiple packages and functions through the full
    /// serialize → load → replay pipeline and assert every response
    /// matches.  Uses realistic nested JSON for simulate responses to
    /// exercise the serde round-trip beyond simple strings.
    #[test]
    fn multi_entry_round_trip_preserves_all_responses() {
        let mut recording = RecordingTransport::new(MockTransport);

        // Package A — deploy, invoke, simulate.
        let deploy_a = recording
            .deploy_contract(Path::new("a.wasm"), "alice", "testnet", "pkg_a")
            .unwrap();
        let xdr_a = recording
            .build_invoke_xdr(&deploy_a, "alice", "testnet", "transfer", &["100".into()], "pkg_a")
            .unwrap();
        let sim_a = recording
            .simulate_transaction(&xdr_a, "pkg_a", "transfer")
            .unwrap();

        // Package B — deploy, invoke with a different function.
        let deploy_b = recording
            .deploy_contract(Path::new("b.wasm"), "bob", "testnet", "pkg_b")
            .unwrap();
        let xdr_b = recording
            .build_invoke_xdr(&deploy_b, "bob", "testnet", "approve", &[], "pkg_b")
            .unwrap();
        let sim_b = recording
            .simulate_transaction(&xdr_b, "pkg_b", "approve")
            .unwrap();

        // Serialize → load → replay.
        let fixture = recording.into_fixture();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        fixture.save(tmp.path()).unwrap();
        let loaded = FixtureFile::load(tmp.path()).unwrap();
        let mut replay = ReplayTransport::new(loaded);

        // Package A assertions.
        assert_eq!(
            replay
                .deploy_contract(Path::new("a.wasm"), "alice", "testnet", "pkg_a")
                .unwrap(),
            deploy_a
        );
        assert_eq!(
            replay
                .build_invoke_xdr(&deploy_a, "alice", "testnet", "transfer", &["100".into()], "pkg_a")
                .unwrap(),
            xdr_a
        );
        assert_eq!(
            replay.simulate_transaction(&xdr_a, "pkg_a", "transfer").unwrap(),
            sim_a
        );

        // Package B assertions.
        assert_eq!(
            replay
                .deploy_contract(Path::new("b.wasm"), "bob", "testnet", "pkg_b")
                .unwrap(),
            deploy_b
        );
        assert_eq!(
            replay
                .build_invoke_xdr(&deploy_b, "bob", "testnet", "approve", &[], "pkg_b")
                .unwrap(),
            xdr_b
        );
        assert_eq!(
            replay.simulate_transaction(&xdr_b, "pkg_b", "approve").unwrap(),
            sim_b
        );
    }

    // ── Fixture version in round-trip ───────────────────────────────────

    /// into_fixture must set the version to FIXTURE_VERSION so that a
    /// freshly recorded fixture always loads successfully.
    #[test]
    fn into_fixture_sets_correct_version() {
        let recording = RecordingTransport::new(MockTransport);
        let fixture = recording.into_fixture();
        assert_eq!(fixture.fixture_version, crate::fixture::FIXTURE_VERSION);
    }

    // ── Simulate response with complex nested JSON ──────────────────────

    /// Verify that a simulate response containing nested objects and
    /// arrays survives the record → serialize → load → replay cycle
    /// without mutation.
    #[test]
    fn round_trip_preserves_complex_simulate_response() {
        // Simulate a realistic Soroban RPC response with nested structure.
        let complex_response = json!({
            "result": {
                "transactionData": "AAAAAQAAAAMAAABhAAAAAAAAAA==",
                "events": [
                    { "type": "contract", "topic": ["transfer"] },
                    { "type": "system" }
                ],
                "cost": {
                    "cpuInsns": "12345",
                    "memBytes": "67890"
                }
            }
        });

        let mut recording = RecordingTransport::new(MockTransport);
        let _ = recording.simulate_transaction("xdr-blob", "pkg", "fn");
        // Override the recorded entry with our complex response.
        let mut fixture = recording.into_fixture();
        fixture
            .entries
            .insert("simulate:pkg:fn".to_string(), complex_response.clone());

        // Serialize → load → replay.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        fixture.save(tmp.path()).unwrap();
        let loaded = FixtureFile::load(tmp.path()).unwrap();
        let mut replay = ReplayTransport::new(loaded);
        let replayed = replay
            .simulate_transaction("xdr-blob", "pkg", "fn")
            .unwrap();
        assert_eq!(replayed, complex_response);
    }

    // ── Concurrent calls to the same function ───────────────────────────

    /// Record the same (package, function) pair via two different XDR
    /// blobs.  The second write overwrites the first in the entries map,
    /// and the replay must return the last-recorded value.
    #[test]
    fn replay_returns_last_recorded_value_for_duplicate_key() {
        let mut recording = RecordingTransport::new(MockTransport);
        let _ = recording.simulate_transaction("xdr-1", "pkg", "fn");
        let _ = recording.simulate_transaction("xdr-2", "pkg", "fn");

        let fixture = recording.into_fixture();
        let mut replay = ReplayTransport::new(fixture);
        // Both calls map to the same key, but replay always looks up by
        // (package, function), so it returns the last-written value.
        let result = replay.simulate_transaction("any-xdr", "pkg", "fn").unwrap();
        assert_eq!(result, json!({ "result": { "ok": true, "fn": "fn" } }));
    }
}
