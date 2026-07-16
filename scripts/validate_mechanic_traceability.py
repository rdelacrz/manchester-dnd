#!/usr/bin/env python3
"""Fail-closed validation for reachable and implementation-only mechanic traces."""

from __future__ import annotations

import hashlib
import json
import re
import sys
from pathlib import Path
from typing import Any


REGISTRY_PATH = Path("content/mechanics/engine-traceability.json")
APPROVED_SRD_51_URL = (
    "https://media.dndbeyond.com/compendium-images/srd/5.1/SRD_CC_v5.1.pdf"
)
APPROVED_SRD_51_DIGEST = (
    "sha256:2504d2a0abb0a4d491a939be4f17910a2dde0312570ab8d208080225ccf0a1f0"
)
CC_BY_40_LEGAL_CODE = "https://creativecommons.org/licenses/by/4.0/legalcode"
RUST_MODULE_PATHS = {
    "encounter": Path("crates/game-core/src/encounter.rs"),
    "hero": Path("crates/game-core/src/hero.rs"),
    "rules_matrix": Path("crates/game-core/src/rules_matrix.rs"),
}
STABLE_ID = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:-]{2,199}$")
DIGEST = re.compile(r"^sha256:[0-9a-f]{64}$")


class DuplicateJsonKey(ValueError):
    pass


def _object_without_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    value: dict[str, Any] = {}
    for key, item in pairs:
        if key in value:
            raise DuplicateJsonKey(f"duplicate JSON key {key!r}")
        value[key] = item
    return value


def _load_json(path: Path, errors: list[str]) -> dict[str, Any] | None:
    try:
        value = json.loads(
            path.read_text(encoding="utf-8"),
            object_pairs_hook=_object_without_duplicate_keys,
        )
    except (OSError, UnicodeError, json.JSONDecodeError, DuplicateJsonKey) as error:
        errors.append(f"{path}: unreadable strict JSON: {error}")
        return None
    if not isinstance(value, dict):
        errors.append(f"{path}: root must be a JSON object")
        return None
    return value


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(128 * 1024), b""):
            digest.update(chunk)
    return f"sha256:{digest.hexdigest()}"


def _manifest_digest(manifest: dict[str, Any]) -> str:
    canonical = dict(manifest)
    canonical.pop("digest", None)
    encoded = json.dumps(
        canonical,
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    ).encode("utf-8")
    return f"sha256:{hashlib.sha256(encoded).hexdigest()}"


def _require_exact_keys(
    value: dict[str, Any], expected: set[str], context: str, errors: list[str]
) -> None:
    actual = set(value)
    if actual != expected:
        missing = sorted(expected - actual)
        unknown = sorted(actual - expected)
        errors.append(f"{context}: schema keys differ; missing={missing}, unknown={unknown}")


def _require_nonempty_text(value: Any, context: str, errors: list[str]) -> None:
    if not isinstance(value, str) or not value.strip():
        errors.append(f"{context}: must be non-empty text")


def _require_unique_text_list(value: Any, context: str, errors: list[str]) -> list[str]:
    if not isinstance(value, list) or any(
        not isinstance(item, str) or not item for item in value
    ):
        errors.append(f"{context}: must be an array of non-empty strings")
        return []
    if len(value) != len(set(value)):
        errors.append(f"{context}: contains duplicate values")
    return value


def _resolve_repo_path(root: Path, relative: Any, context: str, errors: list[str]) -> Path | None:
    if not isinstance(relative, str) or not relative:
        errors.append(f"{context}: missing repository-relative path")
        return None
    candidate = Path(relative)
    if candidate.is_absolute() or ".." in candidate.parts:
        errors.append(f"{context}: path must stay within the repository")
        return None
    resolved = root / candidate
    if not resolved.is_file():
        errors.append(f"{context}: file does not exist: {relative}")
        return None
    return resolved


def _validate_rust_reference(
    root: Path,
    reference: str,
    is_test: bool,
    source_cache: dict[str, str],
    errors: list[str],
) -> None:
    parts = reference.split("::")
    context = f"Rust reference {reference}"
    if len(parts) < 3 or parts[0] != "manchester_dnd_core":
        errors.append(f"{context}: must start with manchester_dnd_core::<module>")
        return
    module = parts[1]
    relative = RUST_MODULE_PATHS.get(module)
    if relative is None:
        errors.append(f"{context}: unrecognized game-core module {module!r}")
        return
    if is_test and (len(parts) != 4 or parts[2] != "tests"):
        errors.append(f"{context}: tests must use <crate>::<module>::tests::<function>")
        return
    if not is_test and "tests" in parts:
        errors.append(f"{context}: implementation reference points into a test module")
        return
    source = source_cache.get(module)
    if source is None:
        try:
            source = (root / relative).read_text(encoding="utf-8")
        except (OSError, UnicodeError) as error:
            errors.append(f"{context}: cannot read {relative}: {error}")
            return
        source_cache[module] = source
    symbol = parts[-1]
    if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", symbol):
        errors.append(f"{context}: invalid Rust symbol token")
        return
    if is_test:
        pattern = re.compile(
            rf"#\[test\]\s*(?:#\[[^\]]+\]\s*)*fn\s+{re.escape(symbol)}\s*\(",
            re.MULTILINE,
        )
    else:
        pattern = re.compile(
            rf"\b(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:const\s+)?fn\s+{re.escape(symbol)}\s*\(|"
            rf"\b(?:pub(?:\([^)]*\))?\s+)?(?:struct|enum|type|const)\s+{re.escape(symbol)}\b",
            re.MULTILINE,
        )
    if not pattern.search(source):
        errors.append(f"{context}: symbol not found in {relative}")


def _validate_pack_integrity(
    root: Path,
    manifest_path: Path,
    manifest: dict[str, Any],
    errors: list[str],
) -> tuple[list[dict[str, Any]], set[str]]:
    if manifest.get("digest") != _manifest_digest(manifest):
        errors.append(f"{manifest_path}: manifest self-digest is stale")
    pack_root = manifest_path.parent
    files = manifest.get("files")
    if not isinstance(files, list):
        errors.append(f"{manifest_path}: files must be an array")
        return [], set()
    content_documents: list[dict[str, Any]] = []
    content_ids: set[str] = set()
    for index, record in enumerate(files):
        context = f"{manifest_path}: files[{index}]"
        if not isinstance(record, dict):
            errors.append(f"{context}: must be an object")
            continue
        relative = record.get("path")
        if not isinstance(relative, str):
            errors.append(f"{context}: path must be text")
            continue
        file_path = _resolve_repo_path(pack_root, relative, context, errors)
        if file_path is None:
            continue
        if record.get("digest") != _sha256(file_path):
            errors.append(f"{context}: indexed digest is stale")
        if record.get("kind") == "content":
            document = _load_json(file_path, errors)
            if document is not None:
                content_documents.append(document)
                for entry in document.get("entries", []):
                    if isinstance(entry, dict) and isinstance(entry.get("id"), str):
                        if entry["id"] in content_ids:
                            errors.append(f"{context}: duplicate content ID {entry['id']}")
                        content_ids.add(entry["id"])
    provenance_path = _resolve_repo_path(
        pack_root,
        manifest.get("provenance_manifest"),
        f"{manifest_path}: provenance_manifest",
        errors,
    )
    provenance: dict[str, Any] | None = None
    if provenance_path is not None:
        if manifest.get("provenance_digest") != _sha256(provenance_path):
            errors.append(f"{manifest_path}: provenance digest is stale")
        provenance = _load_json(provenance_path, errors)
    if provenance is not None:
        by_key = {
            entry.get("provenance_key"): entry
            for entry in provenance.get("entries", [])
            if isinstance(entry, dict) and isinstance(entry.get("provenance_key"), str)
        }
        for record in files:
            if not isinstance(record, dict):
                continue
            linked = by_key.get(record.get("provenance_key"))
            if linked is None:
                errors.append(
                    f"{manifest_path}: no provenance row for {record.get('path')!r}"
                )
            elif (
                linked.get("path") != record.get("path")
                or linked.get("digest") != record.get("digest")
            ):
                errors.append(
                    f"{manifest_path}: provenance path/digest mismatch for {record.get('path')!r}"
                )
    return content_documents, content_ids


def validate_repository(root: Path) -> list[str]:
    root = root.resolve()
    errors: list[str] = []
    registry_path = root / REGISTRY_PATH
    registry = _load_json(registry_path, errors)
    if registry is None:
        return errors
    _require_exact_keys(
        registry,
        {
            "engine_traceability_schema",
            "ruleset_id",
            "pack_manifest_path",
            "content_traceability_path",
            "source_documents",
            "implementation_only_mechanics",
            "implementation_only_capabilities",
        },
        str(registry_path),
        errors,
    )
    if registry.get("engine_traceability_schema") != "engine-mechanic-traceability/v1":
        errors.append(f"{registry_path}: unsupported traceability schema")
    if registry.get("ruleset_id") != "srd-5.1-cc":
        errors.append(f"{registry_path}: ruleset must remain srd-5.1-cc")

    manifest_path = _resolve_repo_path(
        root, registry.get("pack_manifest_path"), "pack_manifest_path", errors
    )
    trace_path = _resolve_repo_path(
        root, registry.get("content_traceability_path"), "content_traceability_path", errors
    )
    if manifest_path is None or trace_path is None:
        return errors
    manifest = _load_json(manifest_path, errors)
    traceability = _load_json(trace_path, errors)
    if manifest is None or traceability is None:
        return errors
    content_documents, content_ids = _validate_pack_integrity(
        root, manifest_path, manifest, errors
    )

    source_documents = registry.get("source_documents")
    if not isinstance(source_documents, list) or not source_documents:
        errors.append(f"{registry_path}: source_documents must be a non-empty array")
        source_documents = []
    source_by_key: dict[str, dict[str, Any]] = {}
    document_keys: set[str] = set()
    source_fields = {
        "document_key",
        "title",
        "version",
        "publisher",
        "official",
        "location",
        "landing_page",
        "digest",
        "license_class",
        "license_id",
        "license_url",
        "required_notice_path",
        "modification_note",
        "source_keys",
    }
    for index, document in enumerate(source_documents):
        context = f"{registry_path}: source_documents[{index}]"
        if not isinstance(document, dict):
            errors.append(f"{context}: must be an object")
            continue
        _require_exact_keys(document, source_fields, context, errors)
        document_key = document.get("document_key")
        if not isinstance(document_key, str) or not STABLE_ID.fullmatch(document_key):
            errors.append(f"{context}: document_key is not a stable identifier")
        elif document_key in document_keys:
            errors.append(f"{context}: duplicate document_key {document_key}")
        else:
            document_keys.add(document_key)
        for field in ("title", "version", "publisher", "location", "modification_note"):
            _require_nonempty_text(document.get(field), f"{context}.{field}", errors)
        digest = document.get("digest")
        if not isinstance(digest, str) or not DIGEST.fullmatch(digest):
            errors.append(f"{context}: digest must be lowercase sha256")
        notice_path = _resolve_repo_path(
            root, document.get("required_notice_path"), f"{context}.required_notice_path", errors
        )
        if notice_path is not None:
            notice = notice_path.read_text(encoding="utf-8")
            if document.get("license_class") == "srd5_1_cc" and (
                "This work includes material taken from the System Reference Document 5.1"
                not in notice
                or CC_BY_40_LEGAL_CODE not in notice
            ):
                errors.append(f"{context}: SRD notice lacks attribution or legal-code link")
        official = document.get("official")
        if official is True:
            if (
                document.get("document_key") != "source-document:official:srd5.1-cc"
                or document.get("version") != "5.1"
                or document.get("location") != APPROVED_SRD_51_URL
                or document.get("digest") != APPROVED_SRD_51_DIGEST
                or document.get("license_class") != "srd5_1_cc"
                or document.get("license_id") != "CC-BY-4.0"
                or document.get("license_url") != CC_BY_40_LEGAL_CODE
            ):
                errors.append(f"{context}: official SRD 5.1 pin/license differs from approval")
        elif official is False:
            local_path = _resolve_repo_path(
                root, document.get("location"), f"{context}.location", errors
            )
            if local_path is not None and document.get("digest") != _sha256(local_path):
                errors.append(f"{context}: local source digest is stale")
            if (
                document.get("license_class") != "original_private_evaluation"
                or document.get("license_id")
                != "LicenseRef-Manchester-Arcana-Private-Evaluation"
                or document.get("license_url") is not None
            ):
                errors.append(f"{context}: project source license classification is inconsistent")
        else:
            errors.append(f"{context}: official must be boolean")
        source_keys = _require_unique_text_list(
            document.get("source_keys"), f"{context}.source_keys", errors
        )
        for source_key in source_keys:
            if not STABLE_ID.fullmatch(source_key):
                errors.append(f"{context}: invalid source key {source_key!r}")
            if source_key in source_by_key:
                errors.append(f"{context}: source key {source_key} is bound more than once")
            else:
                source_by_key[source_key] = document

    source_cache: dict[str, str] = {}
    trace_fields = {
        "mechanic_id",
        "availability",
        "source_key",
        "source_location",
        "license_class",
        "provenance_key",
        "implementation_symbols",
        "test_ids",
        "consuming_content",
        "required_engine_capabilities",
        "modification_note",
    }
    if traceability.get("traceability_schema") != "mechanics-traceability/v1":
        errors.append(f"{trace_path}: unsupported content traceability schema")
    if traceability.get("pack_id") != manifest.get("id") or traceability.get(
        "pack_version"
    ) != manifest.get("version"):
        errors.append(f"{trace_path}: pack identity/version differs from manifest")
    trace_entries = traceability.get("entries")
    if not isinstance(trace_entries, list):
        errors.append(f"{trace_path}: entries must be an array")
        trace_entries = []
    trace_by_id: dict[str, dict[str, Any]] = {}
    reachable_capabilities: set[str] = set()
    for index, trace in enumerate(trace_entries):
        context = f"{trace_path}: entries[{index}]"
        if not isinstance(trace, dict):
            errors.append(f"{context}: must be an object")
            continue
        _require_exact_keys(trace, trace_fields, context, errors)
        mechanic_id = trace.get("mechanic_id")
        if not isinstance(mechanic_id, str) or not STABLE_ID.fullmatch(mechanic_id):
            errors.append(f"{context}: mechanic_id is not stable")
            continue
        if mechanic_id in trace_by_id:
            errors.append(f"{context}: duplicate mechanic_id {mechanic_id}")
        trace_by_id[mechanic_id] = trace
        source_key = trace.get("source_key")
        source_document = source_by_key.get(source_key)
        if source_document is None:
            errors.append(f"{context}: source key {source_key!r} has no versioned document")
        elif trace.get("license_class") != source_document.get("license_class"):
            errors.append(f"{context}: license class differs from source document")
        _require_nonempty_text(trace.get("source_location"), f"{context}.source_location", errors)
        _require_nonempty_text(trace.get("modification_note"), f"{context}.modification_note", errors)
        implementations = _require_unique_text_list(
            trace.get("implementation_symbols"), f"{context}.implementation_symbols", errors
        )
        tests = _require_unique_text_list(trace.get("test_ids"), f"{context}.test_ids", errors)
        consumers = _require_unique_text_list(
            trace.get("consuming_content"), f"{context}.consuming_content", errors
        )
        capabilities = _require_unique_text_list(
            trace.get("required_engine_capabilities"),
            f"{context}.required_engine_capabilities",
            errors,
        )
        if trace.get("availability") == "active":
            if not implementations or not tests or not consumers:
                errors.append(f"{context}: active mechanic lacks implementation, tests, or consumers")
            reachable_capabilities.update(capabilities)
        for consumer in consumers:
            if consumer not in content_ids:
                errors.append(f"{context}: unknown consuming content {consumer}")
        for reference in implementations:
            _validate_rust_reference(root, reference, False, source_cache, errors)
        for reference in tests:
            _validate_rust_reference(root, reference, True, source_cache, errors)

    declared_content: dict[str, dict[str, Any]] = {}
    for document in content_documents:
        for entry in document.get("entries", []):
            if isinstance(entry, dict) and isinstance(entry.get("id"), str):
                declared_content[entry["id"]] = entry
    manifest_capabilities = set(
        _require_unique_text_list(
            manifest.get("required_engine_capabilities"),
            f"{manifest_path}.required_engine_capabilities",
            errors,
        )
    )
    if reachable_capabilities != manifest_capabilities:
        errors.append(
            f"{trace_path}: reachable capability set differs from manifest; "
            f"missing={sorted(manifest_capabilities - reachable_capabilities)}, "
            f"unadvertised={sorted(reachable_capabilities - manifest_capabilities)}"
        )
    for content_id, content in declared_content.items():
        trace = trace_by_id.get(content_id)
        if trace is None:
            errors.append(f"{content_id}: reachable content lacks a mechanic trace")
            continue
        for field in (
            "availability",
            "source_key",
            "license_class",
            "provenance_key",
            "required_engine_capabilities",
        ):
            if trace.get(field) != content.get(field):
                errors.append(f"{content_id}: trace field {field} differs from content")
        for capability in content.get("required_engine_capabilities", []):
            if capability not in manifest_capabilities:
                errors.append(f"{content_id}: requires unadvertised capability {capability}")

    supplemental_fields = {
        "mechanic_id",
        "source_key",
        "source_location",
        "license_class",
        "implementation_symbols",
        "test_ids",
        "modification_note",
    }
    supplemental = registry.get("implementation_only_mechanics")
    if not isinstance(supplemental, list) or not supplemental:
        errors.append(f"{registry_path}: implementation_only_mechanics must be non-empty")
        supplemental = []
    supplemental_by_id: dict[str, dict[str, Any]] = {}
    all_supplemental_symbols: set[str] = set()
    all_supplemental_tests: set[str] = set()
    for index, mechanic in enumerate(supplemental):
        context = f"{registry_path}: implementation_only_mechanics[{index}]"
        if not isinstance(mechanic, dict):
            errors.append(f"{context}: must be an object")
            continue
        _require_exact_keys(mechanic, supplemental_fields, context, errors)
        mechanic_id = mechanic.get("mechanic_id")
        if not isinstance(mechanic_id, str) or not STABLE_ID.fullmatch(mechanic_id):
            errors.append(f"{context}: mechanic_id is not stable")
            continue
        if mechanic_id in supplemental_by_id:
            errors.append(f"{context}: duplicate supplemental mechanic {mechanic_id}")
        supplemental_by_id[mechanic_id] = mechanic
        source_document = source_by_key.get(mechanic.get("source_key"))
        if source_document is None:
            errors.append(f"{context}: source key has no versioned source document")
        elif mechanic.get("license_class") != source_document.get("license_class"):
            errors.append(f"{context}: license class differs from source document")
        if mechanic_id in trace_by_id:
            trace = trace_by_id[mechanic_id]
            for field in ("source_key", "source_location", "license_class"):
                if mechanic.get(field) != trace.get(field):
                    errors.append(f"{context}: supplemental {field} conflicts with content trace")
        _require_nonempty_text(
            mechanic.get("source_location"), f"{context}.source_location", errors
        )
        _require_nonempty_text(
            mechanic.get("modification_note"), f"{context}.modification_note", errors
        )
        implementations = _require_unique_text_list(
            mechanic.get("implementation_symbols"),
            f"{context}.implementation_symbols",
            errors,
        )
        tests = _require_unique_text_list(
            mechanic.get("test_ids"), f"{context}.test_ids", errors
        )
        if not implementations or not tests:
            errors.append(f"{context}: implementation-only mechanic lacks code or tests")
        all_supplemental_symbols.update(implementations)
        all_supplemental_tests.update(tests)
        for reference in implementations:
            _validate_rust_reference(root, reference, False, source_cache, errors)
        for reference in tests:
            _validate_rust_reference(root, reference, True, source_cache, errors)

    capability_fields = {"capability_id", "exposure", "mechanic_ids", "consuming_content"}
    capabilities = registry.get("implementation_only_capabilities")
    if not isinstance(capabilities, list) or not capabilities:
        errors.append(f"{registry_path}: implementation_only_capabilities must be non-empty")
        capabilities = []
    capability_ids: set[str] = set()
    referenced_supplemental: set[str] = set()
    for index, capability in enumerate(capabilities):
        context = f"{registry_path}: implementation_only_capabilities[{index}]"
        if not isinstance(capability, dict):
            errors.append(f"{context}: must be an object")
            continue
        _require_exact_keys(capability, capability_fields, context, errors)
        capability_id = capability.get("capability_id")
        if not isinstance(capability_id, str) or not STABLE_ID.fullmatch(capability_id):
            errors.append(f"{context}: capability_id is not stable")
        elif capability_id in capability_ids:
            errors.append(f"{context}: duplicate capability_id {capability_id}")
        else:
            capability_ids.add(capability_id)
        if capability_id in manifest_capabilities:
            errors.append(f"{context}: implementation-only capability is advertised as reachable")
        if capability.get("exposure") != "implemented_not_exposed":
            errors.append(f"{context}: exposure must be implemented_not_exposed")
        consumers = _require_unique_text_list(
            capability.get("consuming_content"), f"{context}.consuming_content", errors
        )
        if consumers:
            errors.append(f"{context}: unexposed capability cannot claim consuming content")
        mechanic_ids = _require_unique_text_list(
            capability.get("mechanic_ids"), f"{context}.mechanic_ids", errors
        )
        if not mechanic_ids:
            errors.append(f"{context}: capability must map at least one mechanic")
        for mechanic_id in mechanic_ids:
            if mechanic_id not in supplemental_by_id:
                errors.append(f"{context}: unknown supplemental mechanic {mechanic_id}")
            referenced_supplemental.add(mechanic_id)
    unclaimed_mechanics = set(supplemental_by_id) - referenced_supplemental
    if unclaimed_mechanics:
        errors.append(
            f"{registry_path}: implementation-only mechanics lack a capability: "
            f"{sorted(unclaimed_mechanics)}"
        )

    rules_source = source_cache.get("rules_matrix")
    if rules_source is None:
        try:
            rules_source = (root / RUST_MODULE_PATHS["rules_matrix"]).read_text(
                encoding="utf-8"
            )
        except (OSError, UnicodeError) as error:
            errors.append(f"cannot read rules matrix for coverage audit: {error}")
            rules_source = ""
    public_functions = set(re.findall(r"^pub fn\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(", rules_source, re.MULTILINE))
    traced_rule_symbols = {
        reference.rsplit("::", 1)[-1]
        for reference in all_supplemental_symbols
        if reference.startswith("manchester_dnd_core::rules_matrix::")
    }
    traced_rule_symbols.update(
        reference.rsplit("::", 1)[-1]
        for trace in trace_entries
        if isinstance(trace, dict)
        for reference in trace.get("implementation_symbols", [])
        if isinstance(reference, str)
        and reference.startswith("manchester_dnd_core::rules_matrix::")
    )
    if public_functions - traced_rule_symbols:
        errors.append(
            f"{registry_path}: public rules-matrix functions lack mechanic traces: "
            f"{sorted(public_functions - traced_rule_symbols)}"
        )
    rules_tests = set(
        re.findall(
            r"#\[test\]\s*(?:#\[[^\]]+\]\s*)*fn\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(",
            rules_source,
            re.MULTILINE,
        )
    )
    traced_rule_tests = {
        reference.rsplit("::", 1)[-1]
        for reference in all_supplemental_tests
        if reference.startswith("manchester_dnd_core::rules_matrix::tests::")
    }
    traced_rule_tests.update(
        reference.rsplit("::", 1)[-1]
        for trace in trace_entries
        if isinstance(trace, dict)
        for reference in trace.get("test_ids", [])
        if isinstance(reference, str)
        and reference.startswith("manchester_dnd_core::rules_matrix::tests::")
    )
    if rules_tests - traced_rule_tests:
        errors.append(
            f"{registry_path}: rules-matrix tests lack mechanic traces: "
            f"{sorted(rules_tests - traced_rule_tests)}"
        )

    used_source_keys = {
        trace.get("source_key") for trace in trace_entries if isinstance(trace, dict)
    } | {
        mechanic.get("source_key") for mechanic in supplemental if isinstance(mechanic, dict)
    }
    unused_bindings = set(source_by_key) - used_source_keys
    missing_bindings = used_source_keys - set(source_by_key)
    if unused_bindings:
        errors.append(f"{registry_path}: unused source-key bindings: {sorted(unused_bindings)}")
    if missing_bindings:
        errors.append(f"{registry_path}: mechanics lack source bindings: {sorted(missing_bindings)}")
    return errors


def main() -> int:
    repository_root = Path(__file__).resolve().parents[1]
    errors = validate_repository(repository_root)
    if errors:
        print("mechanic traceability gate failed:", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        return 1
    print(
        "mechanic traceability gate passed: reachable capabilities, source pins, "
        "implementation references, and tests are complete"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
