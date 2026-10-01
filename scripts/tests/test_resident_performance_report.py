import importlib.util
from pathlib import Path
import unittest
import tempfile
import hashlib
import json
import io
import sqlite3
import tarfile
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    "resident_performance", Path(__file__).parents[1] / "resident-performance-report.py"
)
REPORT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REPORT)


class ResidentPerformanceReport(unittest.TestCase):
    def write_reference(self, path, value):
        path.write_text(json.dumps(value))
        return {'path': str(path), 'sha256': hashlib.sha256(path.read_bytes()).hexdigest()}

    def write_text_reference(self, path, value):
        path.write_text(value)
        return {'path': str(path), 'sha256': hashlib.sha256(path.read_bytes()).hexdigest()}

    def change_worker_actions(self, packet, transform):
        path = Path(packet['ghc_invocations']['path'])
        rows = [json.loads(line) for line in path.read_text().splitlines()]
        transform(rows)
        packet['ghc_invocations'] = self.write_text_reference(
            path, ''.join(json.dumps(row) + '\n' for row in rows))

    def packet_fixture(self, manifest, binary):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        root = Path(directory.name)
        admitted_compiler = patch.object(REPORT, 'nix_compiler_path', side_effect=lambda path: Path(path) == Path(binary.name))
        admitted_compiler.start()
        self.addCleanup(admitted_compiler.stop)
        archive = root / 'sources.tar'
        helper_bytes = REPORT.GHC_SOURCE_HELPER.read_bytes()
        helper_copy = root / 'ghc_source_options.py'
        helper_copy.write_bytes(helper_bytes)
        source_files = {
            'bridge/haskell/app/Main.hs': b'module Main where\nmain = pure ()\n',
            'bridge/haskell/src/Tidepool/Support.hs': b'module Tidepool.Support where\nsupport = pure ()\n',
            'build/testing/ghc_source_options.py': helper_bytes,
        }
        source_entries = []
        with tarfile.open(archive, 'w') as retained:
            for source_path, source_bytes in source_files.items():
                source_entries.append({'path': source_path, 'kind': 'file', 'bytes': len(source_bytes),
                                       'sha256': hashlib.sha256(source_bytes).hexdigest()})
                member = tarfile.TarInfo(source_path)
                member.size = len(source_bytes)
                retained.addfile(member, io.BytesIO(source_bytes))
        archive_ref = {'path': str(archive), 'sha256': hashlib.sha256(archive.read_bytes()).hexdigest()}
        source = {'head_oid': manifest['source_oid'], 'entries': source_entries}
        base = {
            'source_oid': manifest['source_oid'], 'exit_code': 0,
            'compiler': {'path': binary.name, 'sha256': manifest['binary_sha256']},
            'output': {'path': binary.name, 'sha256': manifest['binary_sha256']},
            'source_before': self.write_reference(root / 'before.json', {'version': 1, 'capture_changes': [], 'source': source, 'archive_sha256': archive_ref['sha256']}),
            'source_after': self.write_reference(root / 'after.json', {'source': source, 'changes': []}),
            'source_archive': archive_ref,
            'source_options_tool': {'path': str(helper_copy), 'sha256': hashlib.sha256(helper_bytes).hexdigest()},
            'build_log': self.write_reference(root / 'build.log', {'exit_code': 0}),
        }
        for kind in REPORT.MIN_COUNTS:
            cargo_path = f'/build/{kind}'
            packet = dict(base, schema='cargo-build-v1', command=['cargo', 'build', '--release'],
                          output=dict(base['output'], cargo_path=cargo_path),
                          cargo_messages=self.write_reference(root / f'{kind}-cargo.jsonl', {
                              'reason': 'compiler-artifact', 'executable': cargo_path,
                              'profile': {'opt_level': '3', 'test': kind != 'cold_start'},
                          }))
            ref = self.write_reference(root / f'{kind}-packet.json', packet)
            manifest['runners'][kind]['rust_build'] = {'packet_path': ref['path'], 'packet_sha256': ref['sha256']}
        build_directory = '/source/target/cabal-build'
        def action(component, target):
            output = f'{build_directory}/{component}'
            return {'argv': [binary.name, '--make', '-O2', '-outputdir', output,
                             '-odir', output, target],
                    'cwd': '/source/bridge/haskell', 'exit_code': 0}
        rows = [action('library', 'Tidepool.Support'), action('executable', 'app/Main.hs'),
                action('executable', 'app/Main.hs')]
        actions = self.write_text_reference(root / 'ghc-invocations.jsonl',
                                           ''.join(json.dumps(row) + '\n' for row in rows))
        log = self.write_text_reference(root / 'ghc-build.log',
            f'[1 of 1] Compiling Tidepool.Support ( src/Tidepool/Support.hs, {build_directory}/library/Tidepool/Support.o )\n'
            f'[1 of 1] Compiling Main ( app/Main.hs, {build_directory}/executable/Main.o )\n')
        worker = dict(base, schema='ghc-build-v2', command=['cabal', 'build'],
                      ghc_invocations=actions, source_root='/source',
                      build_directory=build_directory, build_log=log)
        ref = self.write_reference(root / 'worker-packet.json', worker)
        manifest['worker_build'] = {'packet_path': ref['path'], 'packet_sha256': ref['sha256']}

    def change_packet(self, reference, change):
        path = Path(reference['packet_path'])
        packet = json.loads(path.read_text())
        change(packet)
        path.write_text(json.dumps(packet))
        reference['packet_sha256'] = hashlib.sha256(path.read_bytes()).hexdigest()

    def test_streamed_logs_preserve_prefixed_records_and_reject_truncation(self):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / "samples.log"
            path.write_text('unrelated diagnostic\nresident-performance {"index": 1}\n')
            self.assertEqual(list(REPORT.read_jsonl(path, REPORT.PREFIX)), [{"index": 1}])
            path.write_text('resident-performance \n')
            with self.assertRaises(json.JSONDecodeError):
                list(REPORT.read_jsonl(path, REPORT.PREFIX))
            path.write_text('\n{"index": 2}\n\n')
            self.assertEqual(list(REPORT.read_jsonl(path)), [{"index": 2}])

    def fixture(self):
        manifest = {key: "f" * 64 for key in (
            "source_oid", "command", "binary_sha256", "frontend_sha256",
            "worker_sha256", "compiler_producer",
        )}
        manifest["source_oid"] = "a" * 40
        manifest["command"] = ["frozen-fixture", "--exact"]
        manifest["schema"] = 2
        manifest["exit_code"] = 0
        binary = tempfile.NamedTemporaryFile()
        binary.write(b"frozen pair bytes")
        binary.flush()
        self.addCleanup(binary.close)
        manifest["binary_path"] = binary.name
        for key in ("binary_sha256", "frontend_sha256", "worker_sha256"):
            manifest[key] = hashlib.sha256(b"frozen pair bytes").hexdigest()
        manifest["runners"] = {kind: {
            "binary_path": binary.name, "binary_sha256": manifest["binary_sha256"],
            "command": ["frozen-fixture", "--exact", kind], "profile": "release", "exit_code": 0,
            "expected_test_count": 0 if kind == "cold_start" else 1,
            "executed_test_count": 0 if kind == "cold_start" else 1,
        } for kind in REPORT.MIN_COUNTS}
        manifest["runners"]["cold_start"].update(
            package_entrypoint=binary.name, package_entrypoint_sha256=manifest["binary_sha256"],
            process_execution_count=5,
        )
        self.packet_fixture(manifest, binary)
        events = []
        for index in range(5):
            events.append({"fields": {
                "message": "compiler daemon ready", "daemon_epoch": str(index),
                "producer": "f" * 64, "daemon_pid": 200 + index,
                "executable": binary.name, "worker": binary.name,
            }})
        samples = []
        for index in range(50):
            warm_admission = 1000 + index
            warm_digest = str(index)
            warm_identity = {"daemon_epoch": "0", "admission_id": warm_admission, "request_ordinal": 1,
                             "compile_request": warm_digest}
            events.append({"fields": {"message": "compiler job dequeued", "daemon_epoch": "0",
                                      "admission_id": warm_admission, "queue_ms": 1}})
            events.append({"fields": {"message": "compiler request started", "daemon_epoch": "0",
                                      **warm_identity}})
            events.append({"fields": {
                "message": "compiler request finished", "daemon_epoch": "0", **warm_identity,
                "transport": "daemon", "exit_code": 0, "worker_pid": 300,
                "daemon_pid": 200, "served": index + 1, "followed_rotation": False,
                "elapsed_ms": 10,
            }})
            samples.append({
                "schema": 1, "composition": "engine-store", "kind": "warm_cell",
                "runner_id": "warm_cell",
                "index": index, "elapsed_ns": 100_000_000, "completed": True,
                "displayed": True, "workload": str(index % 10),
                "source_digest": f"{index % 10:064x}",
                "daemon_epoch": "0", "compiler_requests": [warm_identity],
                "operation_id": {"origin":{"kind":"embedded","run":"run","actor":"/root","incarnation":"1"}, "request":f"request-{index}", "call":"reused-call"},
            })
            cancel_operation = {"origin":{"kind":"embedded","run":"run","actor":"/root","incarnation":"1"}, "request":f"cancel-request-{index}", "call":"reused-call"}
            cancel_admission = 2000 + index
            cancel_identity = {"daemon_epoch": "0", "admission_id": cancel_admission, "request_ordinal": 1,
                               "compile_request": f"cancel-{index}"}
            events.append({"fields":{"message":"compiler job dequeued","daemon_epoch":"0","admission_id":cancel_admission,"queue_ms":1}})
            events.append({"fields":{"message":"compiler request started","daemon_epoch":"0",**cancel_identity}})
            events.append({"fields":{"message":"compiler request finished","daemon_epoch":"0",**cancel_identity,"transport":"daemon","exit_code":0,"worker_pid":300,"daemon_pid":200}})
            samples.append({
                "schema": 1, "composition": "engine-store", "kind": "cancel_ack",
                "runner_id": "cancel_ack",
                "index": index, "elapsed_ns": 10_000_000, "completed": True,
                "daemon_epoch": "0", "started_ns": 20_000_000,
                "settled_ns": 30_000_000, "effect_active_ns": 10_000_000,
                "acknowledged": True, "operation_id": json.dumps(cancel_operation), "original_operation":cancel_operation,"compiler_requests":[cancel_identity],
            })
        for index in range(50):
            samples.append({
                "schema": 1, "composition": "engine-store", "kind": "cancel_cleanup",
                "runner_id": "cancel_ack", "index": index, "daemon_epoch": "0",
                "started_ns": 30_000_000, "settled_ns": 40_000_000, "elapsed_ns": 10_000_000,
                "completed": True, "operation_id": samples[2 * index + 1]["operation_id"], "native_abort_confirmed": True,
            })
        for index in range(5):
            samples.append({
                "schema": 1, "composition": "engine-store", "kind": "cold_start",
                "runner_id": "cold_start", "run_id": f"cold-{index}", "provider_requests": 0,
                "index": index, "elapsed_ns": 1_000_000_000, "completed": True,
                "daemon_epoch": str(index), "started_ns": 0,
                "settled_ns": 1_000_000_000, "packaged": True,
                "host_pid": 500 + index, "readiness": "workspace-ready",
                "host_sha256": manifest["binary_sha256"], "actual_retained_host_executable": binary.name,
            })
        return samples, events, manifest

    def test_gate_runners_require_their_actual_optimized_binaries_and_counts(self):
        for field, value in [("binary_sha256", "0" * 64), ("executed_test_count", 0)]:
            samples, events, manifest = self.fixture()
            manifest["runners"]["warm_cell"][field] = value
            self.assertFalse(REPORT.analyze(samples, events, manifest)["accepted"])
        samples, events, manifest = self.fixture()
        samples[0]["runner_id"] = "cold_start"
        self.assertFalse(REPORT.analyze(samples, events, manifest)["accepted"])

    def test_release_label_cannot_admit_debug_cargo_artifact(self):
        samples, events, manifest = self.fixture()
        def change(packet):
            path = Path(packet['cargo_messages']['path'])
            row = json.loads(path.read_text())
            row['profile']['opt_level'] = '0'
            packet['cargo_messages'] = self.write_reference(path, row)
        self.change_packet(manifest['runners']['warm_cell']['rust_build'], change)
        self.assertFalse(REPORT.analyze(samples, events, manifest)['accepted'])

    def test_build_requires_unique_exact_cargo_output_and_test_profile(self):
        for mutation in ('another-output', 'duplicate', 'another-profile'):
            samples, events, manifest = self.fixture()
            def change(packet):
                path = Path(packet['cargo_messages']['path'])
                row = json.loads(path.read_text())
                if mutation == 'another-output':
                    row['executable'] = '/build/unrelated-binary'
                if mutation == 'another-profile':
                    row['profile']['test'] = False
                path.write_text(json.dumps(row) + '\n' + (json.dumps(row) + '\n' if mutation == 'duplicate' else ''))
                packet['cargo_messages']['sha256'] = hashlib.sha256(path.read_bytes()).hexdigest()
            self.change_packet(manifest['runners']['warm_cell']['rust_build'], change)
            self.assertFalse(REPORT.analyze(samples, events, manifest)['accepted'])

    def test_build_source_tool_output_and_logs_are_anchored(self):
        for mutation in ('source', 'exit', 'compiler', 'output', 'log', 'archive', 'moving-source'):
            samples, events, manifest = self.fixture()
            def change(packet):
                if mutation == 'source':
                    packet['source_oid'] = 'b' * 40
                elif mutation == 'exit':
                    packet['exit_code'] = 1
                elif mutation in ('compiler', 'output', 'archive', 'log'):
                    field = {'archive': 'source_archive', 'log': 'build_log'}.get(mutation, mutation)
                    packet[field]['sha256'] = '0' * 64
                else:
                    path = Path(packet['source_after']['path'])
                    row = json.loads(path.read_text())
                    row['source']['entries'] = [{'path': 'changed.hs'}]
                    packet['source_after'] = self.write_reference(path, row)
            self.change_packet(manifest['runners']['warm_cell']['rust_build'], change)
            self.assertFalse(REPORT.analyze(samples, events, manifest)['accepted'])

    def test_missing_build_packet_cannot_be_replaced_with_release_label(self):
        samples, events, manifest = self.fixture()
        manifest['runners']['warm_cell'].pop('rust_build')
        self.assertFalse(REPORT.analyze(samples, events, manifest)['accepted'])

    def test_retained_cold_copies_match_bytes_instead_of_one_path(self):
        samples, events, manifest = self.fixture()
        with tempfile.TemporaryDirectory() as root:
            for row in samples:
                if row["kind"] == "cold_start":
                    retained = Path(root) / row["run_id"]
                    retained.write_bytes(b"frozen pair bytes")
                    row["actual_retained_host_executable"] = str(retained)
            self.assertTrue(REPORT.analyze(samples, events, manifest)["accepted"])
            retained.write_bytes(b"changed retained host")
            self.assertFalse(REPORT.analyze(samples, events, manifest)["accepted"])

    def test_cold_process_identity_idle_start_and_count_are_required(self):
        for field, value in [("host_pid", 500), ("run_id", "cold-0"), ("provider_requests", 1)]:
            samples, events, manifest = self.fixture()
            for row in samples:
                if row["kind"] == "cold_start":
                    row[field] = value
            self.assertFalse(REPORT.analyze(samples, events, manifest)["accepted"])
        samples, events, manifest = self.fixture()
        manifest["runners"]["cold_start"]["process_execution_count"] = 4
        self.assertFalse(REPORT.analyze(samples, events, manifest)["accepted"])

    def test_cleanup_must_pair_every_original_acknowledgment(self):
        samples, events, manifest = self.fixture()
        samples = [row for row in samples if not (row["kind"] == "cancel_cleanup" and row["index"] == 3)]
        self.assertFalse(REPORT.analyze(samples, events, manifest)["accepted"])
        for field, value in [("operation_id", "another"), ("daemon_epoch", "1"), ("started_ns", 31_000_000), ("native_abort_confirmed", False)]:
            samples, events, manifest = self.fixture()
            next(row for row in samples if row["kind"] == "cancel_cleanup")[field] = value
            self.assertFalse(REPORT.analyze(samples, events, manifest)["accepted"])

    def test_optimized_rust_does_not_prove_optimized_haskell(self):
        samples, events, manifest = self.fixture()
        manifest.pop("worker_build")
        self.assertFalse(REPORT.analyze(samples, events, manifest)["accepted"])
        samples, events, manifest = self.fixture()
        manifest["worker_build"]["packet_sha256"] = "0" * 64
        self.assertFalse(REPORT.analyze(samples, events, manifest)["accepted"])

    def test_matched_resident_evidence_is_accepted(self):
        self.assertTrue(REPORT.analyze(*self.fixture())["accepted"])

    def test_missing_worker_evidence_fails_even_with_fast_samples(self):
        samples, events, manifest = self.fixture()
        next(event["fields"] for event in events
             if event["fields"].get("message") == "compiler request finished").pop("worker_pid")
        self.assertFalse(REPORT.analyze(samples, events, manifest)["accepted"])

    def test_cold_worker_cannot_be_counted_as_warm(self):
        samples, events, manifest = self.fixture()
        finish = next(event["fields"] for event in events
                      if event["fields"].get("message") == "compiler request finished")
        finish["served"] = 0
        finish["followed_rotation"] = True
        self.assertFalse(REPORT.analyze(samples, events, manifest)["accepted"])

    def test_neighboring_epoch_or_private_session_cannot_close_product_gate(self):
        samples, events, manifest = self.fixture()
        samples[0]["daemon_epoch"] = "unobserved"
        self.assertFalse(REPORT.analyze(samples, events, manifest)["accepted"])
        samples[0]["composition"] = "private-session"
        self.assertFalse(REPORT.analyze(samples, events, manifest)["accepted"])

    def test_each_cold_start_must_meet_its_limit(self):
        samples, events, manifest = self.fixture()
        samples[-1]["elapsed_ns"] = 10_000_000_001
        self.assertFalse(REPORT.analyze(samples, events, manifest)["accepted"])

    def test_cancel_requires_actual_active_effect_and_unique_operation(self):
        samples, events, manifest = self.fixture()
        samples[1].pop("effect_active_ns")
        self.assertFalse(REPORT.analyze(samples, events, manifest)["accepted"])
        samples[1]["effect_active_ns"] = 10_000_000
        samples[3]["operation_id"] = samples[1]["operation_id"]
        self.assertFalse(REPORT.analyze(samples, events, manifest)["accepted"])

    def test_cold_requires_exact_boundaries_and_packaged_host(self):
        samples, events, manifest = self.fixture()
        samples[-1]["settled_ns"] += 1
        self.assertFalse(REPORT.analyze(samples, events, manifest)["accepted"])
        samples[-1]["settled_ns"] -= 1
        samples[-1]["packaged"] = False
        self.assertFalse(REPORT.analyze(samples, events, manifest)["accepted"])

    def package_cold_report_fixture(self, manifest, samples, root):
        runner = dict(manifest["runners"]["cold_start"])
        runner.update(exit_code=0, process_execution_count=5,
                      command=["python3", "package-cold-start-gate.py", "--output", str(root)],
                      expected_test_count=0, executed_test_count=0,
                      harness_store_schema={"version": 8})
        rows = []
        for sample in [row for row in samples if row["kind"] == "cold_start"]:
            index = sample["index"]
            epoch = str(index)
            trace_path = root / f"compiler-{index}.jsonl"
            store_path = root / f"store-{index}.sqlite"
            store_path.unlink(missing_ok=True)
            with sqlite3.connect(store_path) as connection:
                connection.executescript(
                    "CREATE TABLE schema_version(version INTEGER NOT NULL);"
                    "INSERT INTO schema_version VALUES(8);"
                    "CREATE TABLE decisions(hook TEXT NOT NULL);"
                    "CREATE TABLE events(kind TEXT NOT NULL);")
            provider_evidence = REPORT.COLD_START_GATE.provider_store_evidence(store_path, 8)
            trace = {"fields": {
                "message": "compiler daemon ready", "daemon_epoch": epoch,
                "run_id": sample["run_id"],
                "producer": manifest["compiler_producer"], "daemon_pid": 200 + index,
                "executable": manifest["binary_path"], "worker": manifest["binary_path"],
            }}
            trace_path.write_text(json.dumps(trace) + "\n")
            row = dict(sample)
            row.update(
                init_exit_code=0, stop_exit_code=0, failure=None,
                provider_request_evidence=provider_evidence,
                daemon_epoch=epoch, compiler_producer=manifest["compiler_producer"],
                daemon_pid=200 + index,
                daemon_trace=str(trace_path),
                daemon_trace_sha256=hashlib.sha256(trace_path.read_bytes()).hexdigest(),
            )
            rows.append(row)
        report_path = root / "package-report.json"
        report_path.write_text(json.dumps({
            "schema": 2, "failure": None, "runners": {"cold_start": runner}, "samples": rows,
        }))
        return report_path, rows

    def test_package_cold_report_adapts_actual_samples_and_preserves_build_packet(self):
        samples, events, manifest = self.fixture()
        with tempfile.TemporaryDirectory() as temporary:
            path, source_rows = self.package_cold_report_fixture(manifest, samples, Path(temporary))
            cold_rows, cold_runner, traces = REPORT.package_cold_samples(path, manifest)
            self.assertEqual(len(cold_rows), 5)
            self.assertTrue(all(row["completed"] is True for row in cold_rows))
            self.assertEqual(len(traces), 5)
            self.assertEqual(cold_rows[0]["daemon_trace"], source_rows[0]["daemon_trace"])
            rust_build = manifest["runners"]["cold_start"]["rust_build"]
            merged = REPORT.merge_package_cold_runner(manifest, cold_runner)
            self.assertEqual(merged["runners"]["cold_start"]["rust_build"], rust_build)
            ready_events = [json.loads(line) for trace in traces for line in trace.read_text().splitlines()]
            combined = [row for row in samples if row["kind"] != "cold_start"] + cold_rows
            result = REPORT.analyze(combined, events + ready_events, merged)
            self.assertTrue(result["accepted"], result["evidence_problems"])

    def test_package_cold_adapter_refuses_missing_trace_or_build_provenance(self):
        samples, _, manifest = self.fixture()
        with tempfile.TemporaryDirectory() as temporary:
            path, rows = self.package_cold_report_fixture(manifest, samples, Path(temporary))
            rows[0].pop("daemon_trace_sha256")
            document = json.loads(path.read_text())
            document["samples"] = rows
            path.write_text(json.dumps(document))
            with self.assertRaisesRegex(ValueError, "daemon trace digest"):
                REPORT.package_cold_samples(path, manifest)
            path, _ = self.package_cold_report_fixture(manifest, samples, Path(temporary))
            manifest["runners"]["cold_start"].pop("rust_build")
            with self.assertRaisesRegex(ValueError, "Rust build packet"):
                REPORT.package_cold_samples(path, manifest)

    def test_package_cold_adapter_reopens_store_and_rejects_forged_zero_count(self):
        samples, _, manifest = self.fixture()
        with tempfile.TemporaryDirectory() as temporary:
            path, source_rows = self.package_cold_report_fixture(manifest, samples, Path(temporary))
            first = source_rows[0]
            store_path = Path(first["provider_request_evidence"]["store_path"])
            with sqlite3.connect(store_path) as connection:
                connection.execute("INSERT INTO decisions(hook) VALUES('before-request')")
            actual = REPORT.COLD_START_GATE.provider_store_evidence(store_path, 8)
            forged = dict(actual, provider_request_attempts=0)
            first["provider_request_evidence"] = forged
            first["provider_requests"] = 0
            document = json.loads(path.read_text())
            document["samples"] = source_rows
            path.write_text(json.dumps(document))
            with self.assertRaisesRegex(ValueError, "provider_request_attempts differs from SQLite"):
                REPORT.package_cold_samples(path, manifest)

    def test_package_cold_adapter_rejects_non_sqlite_even_with_matching_file_hash(self):
        samples, _, manifest = self.fixture()
        with tempfile.TemporaryDirectory() as temporary:
            path, source_rows = self.package_cold_report_fixture(manifest, samples, Path(temporary))
            first = source_rows[0]
            store_path = Path(first["provider_request_evidence"]["store_path"])
            store_path.write_bytes(b"not sqlite")
            first["provider_request_evidence"]["files"] = [{
                "path": str(store_path), "sha256": hashlib.sha256(store_path.read_bytes()).hexdigest(),
                "bytes": store_path.stat().st_size,
            }]
            document = json.loads(path.read_text())
            document["samples"] = source_rows
            path.write_text(json.dumps(document))
            with self.assertRaises(REPORT.COLD_START_GATE.GateError):
                REPORT.package_cold_samples(path, manifest)

    def test_main_rejects_cold_rows_without_package_adapter(self):
        samples, _, manifest = self.fixture()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sample_path = root / "samples.jsonl"
            sample_path.write_text("resident-performance " + json.dumps(samples[-1]) + "\n")
            manifest_path = root / "manifest.json"
            manifest_path.write_text(json.dumps(manifest))
            trace_path = root / "trace.jsonl"
            trace_path.write_text("{}\n")
            with patch.object(REPORT.sys, "argv", [
                    "resident-performance-report.py", "--samples", str(sample_path),
                    "--compiler-trace", str(trace_path), "--manifest", str(manifest_path),
                    "--output", str(root / "report.json")]):
                with self.assertRaises(SystemExit) as error:
                    REPORT.main()
            self.assertEqual(error.exception.code, 2)

    def test_package_report_supplies_missing_runner_metadata_but_not_build_packet(self):
        samples, _, manifest = self.fixture()
        with tempfile.TemporaryDirectory() as temporary:
            path, _ = self.package_cold_report_fixture(manifest, samples, Path(temporary))
            merged_input = dict(manifest)
            merged_input["runners"] = dict(manifest["runners"])
            packet_ref = manifest["runners"]["cold_start"]["rust_build"]
            merged_input["runners"]["cold_start"] = {"rust_build": packet_ref}
            _, cold_runner, _ = REPORT.package_cold_samples(path, merged_input)
            merged = REPORT.merge_package_cold_runner(merged_input, cold_runner)
            self.assertEqual(merged["runners"]["cold_start"]["binary_sha256"],
                             manifest["runners"]["cold_start"]["binary_sha256"])
            self.assertEqual(merged["runners"]["cold_start"]["rust_build"], packet_ref)

    def test_report_cli_loads_package_trace_references_and_records_source_digest(self):
        samples, events, manifest = self.fixture()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            package_report, _ = self.package_cold_report_fixture(manifest, samples, root)
            samples_path = root / "samples.jsonl"
            samples_path.write_text("".join(
                "resident-performance " + json.dumps(row) + "\n"
                for row in samples if row["kind"] != "cold_start"))
            trace_path = root / "host-compiler.jsonl"
            trace_path.write_text("".join(json.dumps(event) + "\n" for event in events))
            manifest_path = root / "manifest.json"
            manifest_path.write_text(json.dumps(manifest))
            output_path = root / "report.json"
            with patch.object(REPORT.sys, "argv", [
                    "resident-performance-report.py", "--samples", str(samples_path),
                    "--compiler-trace", str(trace_path), "--manifest", str(manifest_path),
                    "--package-cold-report", str(package_report), "--output", str(output_path)]):
                self.assertEqual(REPORT.main(), 0)
            report = json.loads(output_path.read_text())
            self.assertTrue(report["accepted"], report["evidence_problems"])
            self.assertEqual(report["package_cold_report"]["path"], str(package_report))
            self.assertEqual(report["package_cold_report"]["sha256"],
                             hashlib.sha256(package_report.read_bytes()).hexdigest())

    def test_actual_binary_bytes_and_source_variation_are_required(self):
        samples, events, manifest = self.fixture()
        manifest["frontend_sha256"] = "0" * 64
        self.assertFalse(REPORT.analyze(samples, events, manifest)["accepted"])
        samples, events, manifest = self.fixture()
        for row in samples:
            if row["kind"] == "warm_cell":
                row["source_digest"] = "0" * 64
        self.assertFalse(REPORT.analyze(samples, events, manifest)["accepted"])

    def test_retained_cell_source_must_match_its_digest(self):
        samples, events, manifest = self.fixture()
        for row in samples:
            if row["kind"] == "warm_cell":
                row["source"] = f"({row['index']} + 42 :: Int)"
                row["source_digest"] = hashlib.sha256(row["source"].encode()).hexdigest()
        self.assertTrue(REPORT.analyze(samples, events, manifest)["accepted"])
        samples[0]["source"] = "(0 :: Int)"
        self.assertFalse(REPORT.analyze(samples, events, manifest)["accepted"])

    def test_worker_phase_costs_and_io_counts_remain_separate_from_wall_clock(self):
        samples, events, manifest = self.fixture()
        for line in (
            "tidepool-timing phase=exact_iface_decode ms=12",
            "tidepool-timing phase=exact_iface_decode ms=23",
            "tidepool-count name=exact_iface_decode_reads.Session.Val.G1 count=2",
            "private diagnostic without a machine counter",
        ):
            events.append({"fields": {"message": "compiler timing", "line": line}})
        report = REPORT.analyze(samples, events, manifest)
        self.assertEqual(report["compiler_phases_ms"]["exact_iface_decode"], {
            "count": 2, "total_ms": 35, "p95_ms": 23,
        })
        self.assertEqual(report["compiler_counts"], {"exact_iface_decode_reads.Session.Val.G1": 2})
        self.assertIsNone(report["worker_peak_observed_rss_mb"])
        self.assertTrue(report["accepted"])

    def test_durable_evidence_is_independent_and_requires_retained_manifest(self):
        self.assertEqual(REPORT.analyze_durable([])["status"], "unmeasured")
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / "declarations.json"
            content = json.dumps({"checksum": "retained-checksum", "public_schema": "paired-public-v4"}).encode()
            path.write_bytes(content)
            row = {
                "schema": 1, "composition": "durable-publication", "completed": True,
                "cell": "sample", "elapsed_ns": 100, "certification_ns": None,
                "metadata_stage_file_sync_ns": 10, "publication_rename_directory_sync_ns": 20,
                "manifest_path": str(path), "manifest_bytes": len(content),
                "manifest_checksum": "retained-checksum", "public_schema": "paired-public-v4",
                "manifest_write_bytes": None,
            }
            result = REPORT.analyze_durable([row])
            self.assertEqual(result["status"], "measured")
            self.assertIsNone(result["samples"][0]["manifest_write_bytes"])
            # Current v5 timing is also accepted, but the record and actual
            # retained document must agree on the migration format.
            document = {"checksum": "retained-checksum", "public_schema": "paired-public-v5"}
            content = json.dumps(document).encode()
            path.write_bytes(content)
            row["manifest_bytes"] = len(content)
            self.assertEqual(REPORT.analyze_durable([row])["status"], "invalid")
            row["public_schema"] = "paired-public-v5"
            self.assertEqual(REPORT.analyze_durable([row])["status"], "invalid")
            row.update(checksum_encode_bytes=20, recovery_validation_hash_bytes=30, recovery_materialization_hash_bytes=40, manifest_write_bytes=len(content), inventory_counter_scope="shared-artifact-inventory-owner", artifact_inventory={field:0 for field in ("structural_whole_graph_copies", "reclamation_runs", "reclamation_candidate_nodes", "reclaimed_nodes", "reclamation_elapsed_ns")})
            self.assertEqual(REPORT.analyze_durable([row])["status"], "measured")
            document["public_schema"] = "paired-public-v6"
            content = json.dumps(document).encode()
            path.write_bytes(content)
            row.update(manifest_bytes=len(content), public_schema="paired-public-v6")
            self.assertEqual(REPORT.analyze_durable([row])["status"], "invalid")
            path.unlink()
            self.assertEqual(REPORT.analyze_durable([row])["status"], "invalid")

    def durable_matrix_fixture(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        root = Path(directory.name)
        rows = []
        for baseline in (0, 100):
            for prefix in (1, 10, 100):
                checksum = f"{baseline:03d}-{prefix:03d}"
                document = {"checksum": checksum, "public_schema": "paired-public-v5"}
                content = json.dumps(document).encode()
                path = root / f"declarations-B{baseline}-N{prefix}.json"
                path.write_bytes(content)
                rows.append({
                    "schema": 1, "composition": "durable-publication", "completed": True,
                    "cell": "prefix", "baseline": baseline, "prefix": prefix,
                    "elapsed_ns": 100, "certification_ns": 10,
                    "metadata_stage_file_sync_ns": 20, "publication_rename_directory_sync_ns": 30,
                    "manifest_path": str(path), "manifest_bytes": len(content),
                    "manifest_checksum": checksum, "public_schema": "paired-public-v5",
                    "checksum_encode_bytes": 20, "recovery_validation_hash_bytes": 30,
                    "recovery_materialization_hash_bytes": 40, "manifest_write_bytes": len(content),
                    "inventory_counter_scope": "shared-artifact-inventory-owner",
                    "artifact_inventory": {field: 0 for field in (
                        "structural_whole_graph_copies", "reclamation_runs",
                        "reclamation_candidate_nodes", "reclaimed_nodes", "reclamation_elapsed_ns")},
                })
        return rows

    def test_durable_v5_requires_exact_baseline_by_prefix_matrix(self):
        rows = self.durable_matrix_fixture()
        report = REPORT.analyze_durable(rows)
        self.assertEqual(report["status"], "measured")
        self.assertEqual(report["matrix"]["status"], "measured")
        self.assertEqual(report["matrix"]["observed_count"], 6)
        self.assertEqual(report["matrix"]["missing"], [])
        missing = REPORT.analyze_durable(rows[:-1])
        self.assertEqual(missing["matrix"]["status"], "invalid")
        self.assertIn("B100/N100", missing["matrix"]["missing"])
        self.assertIn("missing durable prefix publication coordinate B100/N100", missing["evidence_problems"])
        duplicated = REPORT.analyze_durable(rows + [rows[0]])
        self.assertEqual(duplicated["matrix"]["status"], "invalid")
        self.assertEqual(duplicated["matrix"]["duplicates"], ["B0/N1"])
        self.assertIn("duplicate durable prefix publication coordinate B0/N1", duplicated["evidence_problems"])
        # A second test execution is not silently collapsed into a replicate:
        # there is no replicate ID or aggregation rule in the existing schema.
        self.assertEqual(duplicated["matrix"]["observed_count"], 6)

    def test_durable_matrix_uses_prefix_cell_counts_not_setup_cells(self):
        rows = self.durable_matrix_fixture()
        setup = dict(rows[0], cell="baseline")
        rows[0] = setup
        report = REPORT.analyze_durable(rows)
        self.assertEqual(report["matrix"]["status"], "invalid")
        self.assertIn("B0/N1", report["matrix"]["missing"])

    def test_compiler_request_cannot_be_charged_to_two_original_operations(self):
        samples, events, manifest = self.fixture()
        samples[2]["compiler_requests"] = samples[0]["compiler_requests"]
        self.assertFalse(REPORT.analyze(samples, events, manifest)["accepted"])
        samples, events, manifest = self.fixture()
        samples[2]["operation_id"] = samples[0]["operation_id"]
        self.assertFalse(REPORT.analyze(samples, events, manifest)["accepted"])

    def test_identical_content_digest_is_valid_for_distinct_invocations(self):
        samples, events, manifest = self.fixture()
        samples[2]["compiler_requests"][0]["compile_request"] = "0"
        for event in events:
            fields = event["fields"]
            if fields.get("admission_id") == 1001 and fields.get("message") in (
                    "compiler request started", "compiler request finished"):
                fields["compile_request"] = "0"
        self.assertTrue(REPORT.analyze(samples, events, manifest)["accepted"])

    def test_digest_only_correlation_is_not_invocation_proof(self):
        samples, events, manifest = self.fixture()
        samples[0]["compiler_requests"] = ["0"]
        report = REPORT.analyze(samples, events, manifest)
        self.assertFalse(report["accepted"])
        self.assertTrue(any("exact admission/request identity" in problem
                            for problem in report["evidence_problems"]))

    def test_exact_invocation_requires_one_start_terminal_and_admission(self):
        for message in ("compiler request started", "compiler request finished",
                        "compiler job dequeued"):
            samples, events, manifest = self.fixture()
            events = [event for event in events if not (
                event["fields"].get("message") == message and
                event["fields"].get("admission_id") == 1000)]
            self.assertFalse(REPORT.analyze(samples, events, manifest)["accepted"], message)
        samples, events, manifest = self.fixture()
        duplicate = next(event for event in events
                         if event["fields"].get("message") == "compiler request finished" and
                         event["fields"].get("admission_id") == 1000)
        events.append(dict(duplicate))
        self.assertFalse(REPORT.analyze(samples, events, manifest)["accepted"])

    def test_worker_requires_actual_ghc_argv_not_a_release_label(self):
        for tail in (['-O0'], ['-O2', '-O0'], ['-O2', '@extra-options'], ['-O2', '-fno-code']):
            with self.subTest(tail=tail):
                samples, events, manifest = self.fixture()
                def change(packet):
                    self.change_worker_actions(packet, lambda rows: rows[0]['argv'].extend(tail))
                self.change_packet(manifest['worker_build'], change)
                self.assertFalse(REPORT.analyze(samples, events, manifest)['accepted'])

    def test_worker_accepts_module_name_library_actions_with_full_source_coverage(self):
        samples, events, manifest = self.fixture()
        self.assertTrue(REPORT.analyze(samples, events, manifest)['accepted'])

    def test_worker_rejects_o0_module_pragma_even_when_recorded_argv_says_o2(self):
        samples, events, manifest = self.fixture()
        packet_ref = manifest['worker_build']
        packet_path = Path(packet_ref['packet_path'])
        packet = json.loads(packet_path.read_text())
        archive_path = Path(packet['source_archive']['path'])
        before_path = Path(packet['source_before']['path'])
        before = json.loads(before_path.read_text())
        source_files = {}
        with tarfile.open(archive_path, 'r:') as old_archive:
            for member in old_archive:
                stream = old_archive.extractfile(member)
                if stream is not None:
                    source_files[member.name] = stream.read()
        source_files['bridge/haskell/app/Main.hs'] = (
            b'{-# OPTIONS_GHC -O0 #-}\nmodule Main where\nmain = pure ()\n')
        entries = []
        with tarfile.open(archive_path, 'w') as retained:
            for source_path, source_bytes in source_files.items():
                entries.append({'path': source_path, 'kind': 'file', 'bytes': len(source_bytes),
                                'sha256': hashlib.sha256(source_bytes).hexdigest()})
                member = tarfile.TarInfo(source_path)
                member.size = len(source_bytes)
                retained.addfile(member, io.BytesIO(source_bytes))
        archive_sha = hashlib.sha256(archive_path.read_bytes()).hexdigest()
        packet['source_archive']['sha256'] = archive_sha
        label_path = packet_path.parent / 'forged-source-options.json'
        packet['source_options'] = self.write_reference(label_path, {
            'schema': 'ghc-source-options-v1', 'status': 'passed',
        })
        before['archive_sha256'] = archive_sha
        before['source']['entries'] = entries
        before_path.write_text(json.dumps(before))
        packet['source_before']['sha256'] = hashlib.sha256(before_path.read_bytes()).hexdigest()
        after_path = Path(packet['source_after']['path'])
        after = json.loads(after_path.read_text())
        after['source']['entries'] = entries
        after_path.write_text(json.dumps(after))
        packet['source_after']['sha256'] = hashlib.sha256(after_path.read_bytes()).hexdigest()
        packet_path.write_text(json.dumps(packet))
        packet_ref['packet_sha256'] = hashlib.sha256(packet_path.read_bytes()).hexdigest()
        self.assertFalse(REPORT.analyze(samples, events, manifest)['accepted'])

    def test_worker_requires_contributing_entrypoint_compilation(self):
        samples, events, manifest = self.fixture()
        def change(packet):
            path = Path(packet['build_log']['path'])
            packet['build_log'] = self.write_text_reference(
                path, '\n'.join(line for line in path.read_text().splitlines() if 'Compiling Main ' not in line))
        self.change_packet(manifest['worker_build'], change)
        self.assertFalse(REPORT.analyze(samples, events, manifest)['accepted'])

    def test_worker_all_contributing_actions_require_effective_optimization(self):
        samples, events, manifest = self.fixture()
        def change(packet):
            self.change_worker_actions(packet, lambda rows: rows[0]['argv'].append('-O0'))
        self.change_packet(manifest['worker_build'], change)
        self.assertFalse(REPORT.analyze(samples, events, manifest)['accepted'])

    def test_worker_resolves_recorded_source_from_its_own_action_cwd(self):
        for cwd, source, accepted in (
                ('/source/bridge/haskell', 'app/Main.hs', True),
                ('/source/bridge', 'haskell/app/Main.hs', True),
                ('/source/unrelated', 'app/Main.hs', False),
                (None, 'bridge/haskell/app/Main.hs', False),
                ('relative/bridge/haskell', 'app/Main.hs', False)):
            with self.subTest(cwd=cwd, source=source):
                samples, events, manifest = self.fixture()
                def change(packet):
                    def change_rows(rows):
                        for row in rows[1:]:
                            row['cwd'] = cwd
                            row['argv'][-1] = source
                    self.change_worker_actions(packet, change_rows)
                    path = Path(packet['build_log']['path'])
                    packet['build_log'] = self.write_text_reference(
                        path, path.read_text().replace('app/Main.hs,', source + ','))
                self.change_packet(manifest['worker_build'], change)
                self.assertEqual(REPORT.analyze(samples, events, manifest)['accepted'], accepted)

    def actor_fixture(self):
        _, events, manifest = self.fixture()
        manifest["runners"]["actor_workload"] = dict(manifest["runners"]["warm_cell"])
        rows = []
        gauges = ("native_functions", "native_code_bytes", "live_old_bytes", "major_collections", "programs", "block_words", "persistent_roots", "handles", "code_exports", "parked", "static_regions", "descriptor_rows", "callable_rows", "enter_rows")
        for actor in range(8):
            for sequence in range(10):
                correlation = f"actor-{actor}-{sequence}"
                admission = 3000 + actor * 10 + sequence
                invocation = {"daemon_epoch":"0","admission_id":admission,"request_ordinal":1,"compile_request":correlation}
                events.append({"fields":{"message":"compiler job dequeued","daemon_epoch":"0","admission_id":admission,"queue_ms":1}})
                events.append({"fields":{"message":"compiler request started","daemon_epoch":"0",**invocation}})
                events.append({"fields":{"message":"compiler request finished","daemon_epoch":"0",**invocation,"worker_pid":300,"daemon_pid":200,"transport":"daemon","exit_code":0}})
                source = f"({sequence} + 42 :: Int)"
                rows.append({"schema":1,"composition":"engine-store-eight-actor","runner_id":"actor_workload","actor_index":actor,"sequence":sequence,"operation_id":{"origin":{"kind":"embedded","run":"run","actor":f"/root/a{actor}_i1","incarnation":"1"},"request":f"request-{actor}-{sequence}","call":f"reused-call-{sequence}"},"source":source,"source_digest":hashlib.sha256(source.encode()).hexdigest(),"elapsed_ns":100,"completed":True,"displayed":True,"typed_reply_confirmed":True,"owner_cleanup_confirmed":True,"daemon_epoch":"0","compiler_requests":[invocation],"machine_owner":{"actor":{"id":actor+1,"incarnation":1},"session":"shared-session"},"machine":{field:1 for field in gauges},"counter_scope":"exact-session-gauges-at-display-wave"})
        return rows, events, manifest

    def test_eight_real_actor_matrix_is_separate_and_exact(self):
        self.assertEqual(REPORT.analyze_actors([], [], {})["status"], "unmeasured")
        rows, events, manifest = self.actor_fixture()
        self.assertEqual(REPORT.analyze_actors(rows, events, manifest)["status"], "measured")
        self.assertEqual(REPORT.analyze_actors(rows[:-1], events, manifest)["status"], "invalid")
        rows[-1]["operation_id"]["origin"] = rows[0]["operation_id"]["origin"]
        self.assertEqual(REPORT.analyze_actors(rows, events, manifest)["status"], "invalid")

    def test_eight_actor_attribution_refuses_shared_request_and_unavailable_gauges(self):
        rows, events, manifest = self.actor_fixture()
        rows[1]["compiler_requests"] = rows[0]["compiler_requests"]
        self.assertEqual(REPORT.analyze_actors(rows, events, manifest)["status"], "invalid")
        rows, events, manifest = self.actor_fixture()
        rows[0]["machine"]["native_code_bytes"] = None
        self.assertEqual(REPORT.analyze_actors(rows, events, manifest)["status"], "invalid")
        rows, events, manifest = self.actor_fixture()
        rows[0]["owner_cleanup_confirmed"] = False
        self.assertEqual(REPORT.analyze_actors(rows, events, manifest)["status"], "invalid")

    def test_nearest_rank_and_empty_evidence(self):
        self.assertEqual(REPORT.percentile(list(range(1, 51)), 0.95), 48)
        self.assertIsNone(REPORT.percentile([], 0.95))
        self.assertFalse(REPORT.analyze([], [], {})["accepted"])


if __name__ == "__main__":
    unittest.main()
