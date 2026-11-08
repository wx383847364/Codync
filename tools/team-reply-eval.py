#!/usr/bin/env python3
"""定点交付评测：配置预检、原验收回归和纯程序提取；不连接模型或提供新的 bot 工具。"""

import argparse
import copy
import json
import re
import sys
import time
import unittest
from pathlib import Path


BOT_FIELDS = {
    "name", "description", "backend", "command", "cwd", "model", "modelOptions",
    "fastMode", "mode", "permission", "connectors", "skills", "notify", "sharedMemory",
    "manageTeam", "fallbacks", "selfFallbacks",
}
RECORD_FIELDS = {"id", "status", "evidence", "duration_ms", "details"}


def checked_object(pairs):
    """拒绝重复 JSON 键，防止源报告或结构检查静默选择被覆盖的值。"""
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON key: " + key)
        result[key] = value
    return result


def reject_constant(value):
    """JSON 报告不接受 NaN/Infinity，不能把非标准数值当作有效证据。"""
    raise ValueError("non-JSON number: " + value)


def load_json(text):
    """严格解析一个 JSON 值，不做类型转换、字符串规范化或尾部文本修复。"""
    return json.loads(text, object_pairs_hook=checked_object, parse_constant=reject_constant)


def record_shape(record):
    """只检查本评测的五字段合同；类型合法不代表字符串内容与源记录相同。"""
    return (
        isinstance(record, dict) and set(record) == RECORD_FIELDS
        and all(type(record[key]) is str for key in RECORD_FIELDS - {"duration_ms"})
        and type(record["duration_ms"]) is int
    )


def read_source(text, report_id, record_id="T025"):
    """解析已知 JSONL 源，固定报告身份并拒绝缺记录、重复 ID 和字段畸形。"""
    # JSONL 只用 LF 分隔；字符串里的 U+2028/U+2029/U+0085 不能被 splitlines 当成新记录。
    lines = text.split("\n")
    if lines[-1] == "":
        lines.pop()
    values = [load_json(line) for line in lines]
    if not values or not isinstance(values[0], dict) or values[0].get("report_id") != report_id:
        raise ValueError("source report_id mismatch")
    if type(report_id) is not str or type(record_id) is not str:
        raise ValueError("report_id and record_id must be strings")
    records = {}
    for record in values[1:]:
        if not record_shape(record):
            raise ValueError("invalid source record")
        identifier = record["id"]
        if identifier in records:
            raise ValueError("duplicate record id: " + identifier)
        records[identifier] = record
    if record_id not in records:
        raise ValueError("source record unavailable: " + record_id)
    return records


def extract(text, report_id, record_id="T025"):
    """从已知源直接投影完整记录和原四字段答案，不调用模型或改写原字段值。"""
    record = read_source(text, report_id, record_id)[record_id]
    delivery = {"report_id": report_id, "record": record}
    answer = {"report_id": report_id, **{key: record[key] for key in ("id", "status", "evidence")}}
    return {"delivery": delivery, "answer": answer}


def objects(text):
    """沿用历史验收的 JSON 对象扫描，容忍 Markdown；严格格式遵从另列。"""
    result = []
    decoder = json.JSONDecoder()
    position = 0
    while position < len(text):
        at = text.find("{", position)
        if at < 0:
            break
        try:
            value, length = decoder.raw_decode(text[at:])
            result.append(value)
            position = at + length
        except ValueError:
            position = at + 1
    return result


def original_evidence_valid(reply, records, report_id, record_id):
    """保留历史完整记录和报告标识谓词，不用正确主脑答案替代执行者证据验收。"""
    found = {}
    headers = []
    stack = objects(reply)
    while stack:
        value = stack.pop()
        if isinstance(value, dict):
            if "report_id" in value:
                headers.append(value)
            key = value.get("id")
            if isinstance(key, str) and key in records:
                if value != records[key]:
                    return False
                found[key] = value
            stack.extend(value.values())
        elif isinstance(value, list):
            stack.extend(value)
    # 沿用已修正的历史计分：报告 ID 可以写在正文，不要求 JSON 键或 report_id 标签。
    marked = any(header["report_id"] == report_id for header in headers) or re.search(
        r"(?<![A-Za-z0-9_-])" + re.escape(report_id) + r"(?![A-Za-z0-9_-])", reply
    )
    return bool(marked) and found.get(record_id) == records[record_id]


def evaluate(text, reply, answer, report_id, record_id="T025"):
    """按原金标分别验收答案与完整原记录；新增严格格式检查不改变历史总分。"""
    records = read_source(text, report_id, record_id)
    expected = extract(text, report_id, record_id)["answer"]
    try:
        # 与历史评分一致：比较首个最终 JSON，不从错误 evidence 对象中提取正确值。
        final = json.JSONDecoder().raw_decode(answer[answer.index("{"):].strip())[0]
        answer_ok = final == expected
    except ValueError:
        answer_ok = False
    evidence_ok = original_evidence_valid(reply, records, report_id, record_id)
    try:
        value = load_json(reply)
        shape_ok = (
            isinstance(value, dict) and set(value) == {"report_id", "record"}
            and value["report_id"] == report_id and record_shape(value["record"])
        )
    except ValueError:
        shape_ok = False
    return {
        "passed": answer_ok and evidence_ok, "coordinator_answer_passed": answer_ok,
        "worker_evidence_passed": evidence_ok, "worker_contract_passed": shape_ok,
    }


def create_checked_bot(call, config):
    """只供评测使用：收费派发前验证 Host API 实际配置，失败仅清理本次创建的 bot。"""
    if set(config) != BOT_FIELDS:
        raise ValueError("evaluation config fields mismatch; Host API uses description, not instructions")
    if type(config["description"]) is not str or not config["description"].strip():
        raise ValueError("standing description required")
    actual = call("createBot", copy.deepcopy(config))["bot"]
    identifier = actual.get("id")
    if not isinstance(identifier, str) or not identifier:
        raise ValueError("created bot id unavailable; evaluation cannot dispatch")
    mismatched = [
        key for key, value in config.items()
        if key not in actual or type(actual[key]) is not type(value) or actual[key] != value
    ]
    if mismatched:
        # 尚未派发任务；清理错误配置的测试 bot，不能触及已有用户 bot。
        call("deleteBot", {"botId": identifier})
        raise ValueError("persisted evaluation config mismatch: " + ", ".join(sorted(mismatched)))
    return actual


class RegressionTests(unittest.TestCase):
    """离线注入历史类型错误和 UI 漏词，保护验收器；不假装证明模型遵守提示。"""

    def setUp(self):
        """固定独立原金标，保留真实反例的重复词和 Unicode 内容。"""
        self.report_id = "r2-point-fresh"
        self.record = {
            "id": "T025", "status": "PASS", "evidence": "T8ZQQXWJM2", "duration_ms": 405,
            "details": "server stable result queue assertion transport validation snapshot client request revision "
                       "client server client snapshot assertion boundary assertion assertion session request validation "
                       "assertion queue cleanup queue local encoding client request local session result response request",
        }
        self.answer = {"report_id": self.report_id, "id": "T025", "status": "PASS", "evidence": "T8ZQQXWJM2"}
        self.source = json.dumps({"report_id": self.report_id}) + "\n" + json.dumps(self.record) + "\n"
        self.delivery = {"report_id": self.report_id, "record": copy.deepcopy(self.record)}
        self.calls = []
        self.config = dict.fromkeys(BOT_FIELDS)
        self.config.update({
            "name": "evaluation", "description": "按本次明确合同交付", "backend": "codex",
            "command": "unused offline", "cwd": "/unused/offline", "model": "gpt-6-luna",
            "modelOptions": {"reasoning_effort": "low"}, "fastMode": False, "mode": "read-only",
            "permission": "ask", "connectors": [], "skills": [], "notify": False,
            "sharedMemory": False, "manageTeam": False, "fallbacks": [], "selfFallbacks": False,
        })
        self.actual = {**copy.deepcopy(self.config), "id": "new-evaluation-bot"}

    def call(self, method, body):
        """只记录配置预检调用；测试期间不能启动真实模型或网络。"""
        self.calls.append((method, body))
        return {"bot": self.actual} if method == "createBot" else {}

    def score(self):
        """使用独立源和金标评分当前被注入的输出。"""
        return evaluate(self.source, json.dumps(self.delivery), json.dumps(self.answer), self.report_id)

    def test_program_extracts_original_complete_record_and_answer(self):
        """程序提取必须与手写原金标一致，包括完整重复词，不能只验 id/evidence。"""
        result = extract(self.source, self.report_id)
        self.assertEqual(result, {"delivery": self.delivery, "answer": self.answer})
        self.assertTrue(self.score()["passed"])

    def test_object_evidence_is_rejected_even_if_it_contains_the_correct_value(self):
        """历史主脑把字符串替换为对象时，不能静默提取内部正确值后判通过。"""
        self.answer["evidence"] = {"evidence": "T8ZQQXWJM2", "duration_ms": 405}
        result = self.score()
        self.assertTrue(result["worker_evidence_passed"])
        self.assertFalse(result["passed"])

    def test_ui_missing_repeated_word_fails_despite_correct_types_and_final_answer(self):
        """复现有效 UI 反例：少一个 assertion 必须拒绝，类型和最终答案不能替代保真。"""
        self.delivery["record"]["details"] = self.record["details"].replace("assertion assertion", "assertion")
        result = self.score()
        self.assertTrue(result["worker_contract_passed"])
        self.assertTrue(result["coordinator_answer_passed"])
        self.assertFalse(result["passed"])

    def test_missing_report_id_fails_with_correct_record_and_answer(self):
        """缺报告标识与历史旧简短组一样判失败，不能从主脑答案反补证据身份。"""
        del self.delivery["report_id"]
        self.assertFalse(self.score()["passed"])

    def test_markdown_remains_accepted_by_original_score_but_not_strict_shape(self):
        """保持原评分容忍的 Markdown 包装，严格格式仅作为单列观察。"""
        result = evaluate(self.source, "```json\n" + json.dumps(self.delivery) + "\n```",
                          json.dumps(self.answer), self.report_id)
        self.assertTrue(result["passed"])
        self.assertFalse(result["worker_contract_passed"])

    def test_unicode_values_are_not_normalized_or_shortened(self):
        """原样 Unicode 分隔符、组合字符、换行和空格按源值保留，不能当作 JSONL 行边界。"""
        self.record["details"] = "中文🙂 e\u0301 é\n重复 重复  \u2028\u2029\u0085"
        self.source = json.dumps({"report_id": self.report_id}) + "\n" + json.dumps(self.record, ensure_ascii=False)
        self.assertEqual(extract(self.source, self.report_id)["delivery"]["record"], self.record)

    def test_original_prose_report_identity_preserves_historical_acceptance(self):
        """原合同允许正文标注报告 ID，不能再次错误要求 report_id 标签来改变历史分数。"""
        reply = "The report is `r2-point-fresh`.\n```json\n" + json.dumps(self.record) + "\n```"
        result = evaluate(self.source, reply, json.dumps(self.answer), self.report_id)
        self.assertTrue(result["passed"])
        self.assertFalse(result["worker_contract_passed"])
        self.assertFalse(evaluate(self.source, reply.replace("r2-point-fresh", "r2-point-fresh-extra"),
                                  json.dumps(self.answer), self.report_id)["passed"])

    def test_normal_line_endings_work_but_empty_records_are_rejected(self):
        """支持正常 LF/CRLF 和末尾单换行，不能悄悄跳过空记录行。"""
        for source in (self.source, self.source.rstrip("\n"), self.source.replace("\n", "\r\n")):
            self.assertEqual(extract(source, self.report_id)["answer"], self.answer)
        with self.assertRaises(ValueError):
            extract(self.source + "\n", self.report_id)

    def test_ambiguous_or_malformed_source_is_rejected(self):
        """重复 ID/键、缺目标、错误报告、截断和类型错误均不能猜测恢复。"""
        invalid = [self.source + json.dumps(self.record), self.source[:-4],
                   '{"report_id":"old","report_id":"r2-point-fresh"}\n' + json.dumps(self.record),
                   json.dumps({"report_id": "other"}) + "\n" + json.dumps(self.record),
                   json.dumps({"report_id": self.report_id}),
                   self.source.replace('"duration_ms": 405', '"duration_ms": true')]
        for source in invalid:
            with self.subTest(source=source), self.assertRaises(ValueError):
                extract(source, self.report_id)

    def test_unsupported_instructions_fails_before_bot_creation(self):
        """错误 instructions 参数在 createBot 前失败，不能进入收费派发。"""
        self.config["instructions"] = self.config.pop("description")
        with self.assertRaises(ValueError):
            create_checked_bot(self.call, self.config)
        self.assertEqual(self.calls, [])

    def test_missing_persisted_setting_cleans_up_only_new_bot_without_dispatch(self):
        """Host 未保存任意实验配置时清理新 bot，调用链中不能出现 send/teamCall。"""
        for key in ("description", "model", "modelOptions", "fastMode", "permission", "mode"):
            with self.subTest(key=key):
                self.calls.clear()
                actual = copy.deepcopy(self.actual)
                del self.actual[key]
                with self.assertRaises(ValueError):
                    create_checked_bot(self.call, self.config)
                self.assertEqual([method for method, _ in self.calls], ["createBot", "deleteBot"])
                self.assertEqual(self.calls[-1][1], {"botId": "new-evaluation-bot"})
                self.actual = actual

    def test_valid_settings_are_checked_before_returning_a_dispatchable_bot(self):
        """有效配置返回新 bot，但预检自身不发任务、不更改用户常驻配置。"""
        self.assertEqual(create_checked_bot(self.call, self.config), self.actual)
        self.assertEqual([method for method, _ in self.calls], ["createBot"])


def main():
    """运行无模型回归或本地程序提取/评分；不隐式连接 Host、发任务或计零提供方用量。"""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--source", type=Path)
    parser.add_argument("--report-id")
    parser.add_argument("--record-id", default="T025")
    parser.add_argument("--reply", type=Path)
    parser.add_argument("--answer", type=Path)
    args = parser.parse_args()
    if args.self_test:
        result = unittest.TextTestRunner(verbosity=2).run(unittest.defaultTestLoader.loadTestsFromTestCase(RegressionTests))
        return 0 if result.wasSuccessful() else 1
    if not args.source or not args.report_id or bool(args.reply) != bool(args.answer):
        parser.error("--source and --report-id required; --reply and --answer must be supplied together")
    started = time.perf_counter()
    try:
        text = args.source.read_text(encoding="utf-8")
        if args.reply:
            result = evaluate(text, args.reply.read_text(encoding="utf-8"),
                              args.answer.read_text(encoding="utf-8"), args.report_id, args.record_id)
        else:
            result = extract(text, args.report_id, args.record_id)
            result.update({"model_invocations": 0, "provider_usage": None,
                           "elapsed_seconds": time.perf_counter() - started})
        print(json.dumps(result, ensure_ascii=False, indent=2))
        return 0 if result.get("passed", True) else 1
    except (ValueError, OSError) as error:
        print(json.dumps({"error": str(error)}, ensure_ascii=False), file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
