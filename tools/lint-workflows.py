#!/usr/bin/env python3
"""Offline sanity checks for .github/workflows/*.yml (no network, no actionlint needed).

    python tools/lint-workflows.py            # all workflows
    python tools/lint-workflows.py FILE...    # only these

Needs PyYAML (`pip install pyyaml`). Exit code 1 when any check fails. Checks:

  * the file parses, has `on`, `jobs`, and every job has `runs-on` and `steps` (or `uses`)
  * every `uses:` of a remote action is pinned to a full 40-hex commit SHA
  * no `pull_request_target` trigger (it runs fork code with repository secrets)
  * `permissions` is set at the top level or on every job; `contents: write` only where a job
    says it publishes (allow-listed below per workflow)
  * no script injection: `run:` blocks never interpolate attacker-influenced contexts
    (github.event.*, github.head_ref, inputs.*, needs.*.outputs.*, steps.*.outputs.*) - those go
    through `env:` instead
  * `${{ secrets.* }}` only appears in `env:` maps (step or job), never in `run:`/`with:`/`if:`
  * every `${{ ... }}` has balanced braces/quotes/parentheses and only names known contexts
  * `needs:` refers to existing jobs, and `needs.<job>.outputs.<x>` / `steps.<id>.outputs.<x>`
    refer to a declared job / step id
"""
from __future__ import annotations

import pathlib
import re
import sys

try:
    import yaml
except ImportError:  # pragma: no cover
    sys.exit("lint-workflows: PyYAML is missing (pip install pyyaml)")

ROOT = pathlib.Path(__file__).resolve().parent.parent
WORKFLOWS = ROOT / ".github" / "workflows"

# Jobs that may hold `contents: write` (they publish a release). Everything else must not.
WRITE_JOBS = {
    "release.yml": {"release", "<top>"},
    "nightly.yml": {"publish"},
}

CONTEXTS = {"github", "env", "vars", "job", "jobs", "steps", "runner", "secrets", "strategy",
            "matrix", "needs", "inputs"}
FUNCS = {"contains", "startsWith", "endsWith", "format", "join", "toJSON", "fromJSON",
         "hashFiles", "success", "always", "cancelled", "failure"}
# Contexts whose value an outsider (or a commit message, a branch name, a step's output) can
# shape. Never interpolate these into a shell script.
UNSAFE_IN_RUN = [
    re.compile(r"\bgithub\.event\b"),
    re.compile(r"\bgithub\.head_ref\b"),
    re.compile(r"\binputs\."),
    re.compile(r"\bneeds\.[\w-]+\.outputs\b"),
    re.compile(r"\bsteps\.[\w-]+\.outputs\b"),
]
EXPR = re.compile(r"\$\{\{(.*?)\}\}", re.S)
SHA_PIN = re.compile(r"^[\w.-]+/[\w./-]+@[0-9a-f]{40}$")


class Lint:
    def __init__(self, name: str):
        self.name = name
        self.errors: list[str] = []

    def err(self, where: str, msg: str) -> None:
        self.errors.append(f"{self.name}: {where}: {msg}")


def walk(node, path=""):
    """Yield (path, key, value) for every scalar string in the document."""
    if isinstance(node, dict):
        for k, v in node.items():
            p = f"{path}.{k}" if path else str(k)
            if isinstance(v, str):
                yield p, str(k), v
            else:
                yield from walk(v, p)
    elif isinstance(node, list):
        for i, v in enumerate(node):
            p = f"{path}[{i}]"
            if isinstance(v, str):
                yield p, "", v
            else:
                yield from walk(v, p)


def strip_strings(expr: str) -> str | None:
    """Remove '...' literals (with '' escapes); None if a quote is unbalanced."""
    out, i = [], 0
    while i < len(expr):
        c = expr[i]
        if c == "'":
            j = i + 1
            while True:
                j = expr.find("'", j)
                if j < 0:
                    return None
                if j + 1 < len(expr) and expr[j + 1] == "'":
                    j += 2
                    continue
                break
            out.append("''")
            i = j + 1
        else:
            out.append(c)
            i += 1
    return "".join(out)


def check_expr(lint: Lint, where: str, expr: str) -> None:
    body = strip_strings(expr)
    if body is None:
        lint.err(where, f"unbalanced quote in ${{{{{expr.strip()}}}}}")
        return
    depth = 0
    for c in body:
        depth += {"(": 1, ")": -1, "[": 1, "]": -1}.get(c, 0)
        if depth < 0:
            break
    if depth != 0:
        lint.err(where, f"unbalanced brackets in ${{{{{expr.strip()}}}}}")
    if "{" in body or "}" in body:
        lint.err(where, f"stray brace in ${{{{{expr.strip()}}}}}")
    for m in re.finditer(r"(?<![\w.'])([A-Za-z_][\w-]*)\s*(\()?", body):
        word, call = m.group(1), m.group(2)
        if call:
            if word not in FUNCS:
                lint.err(where, f"unknown function {word}()")
        elif word not in CONTEXTS and word not in {"true", "false", "null"}:
            lint.err(where, f"unknown context {word!r} in ${{{{{expr.strip()}}}}}")


def lint_file(path: pathlib.Path) -> list[str]:
    lint = Lint(path.name)
    text = path.read_text(encoding="utf-8")
    try:
        # BaseLoader keeps `on:` as the string "on" (YAML 1.1 would make it True)
        doc = yaml.load(text, Loader=yaml.BaseLoader)
    except yaml.YAMLError as e:
        return [f"{path.name}: YAML does not parse: {e}"]
    if not isinstance(doc, dict):
        return [f"{path.name}: not a mapping"]

    on = doc.get("on")
    if on is None:
        lint.err("top", "no `on:`")
    triggers = set(on) if isinstance(on, (dict, list)) else {on} if isinstance(on, str) else set()
    if "pull_request_target" in triggers:
        lint.err("on", "pull_request_target is not allowed (fork code with repository secrets)")

    jobs = doc.get("jobs")
    if not isinstance(jobs, dict) or not jobs:
        lint.err("top", "no jobs")
        return lint.errors

    allowed_write = WRITE_JOBS.get(path.name, set())
    top_perm = doc.get("permissions")
    if isinstance(top_perm, dict) and top_perm.get("contents") == "write" and "<top>" not in allowed_write:
        lint.err("permissions", "top-level contents: write; give it only to the publishing job")
    if top_perm == "write-all":
        lint.err("permissions", "write-all is not allowed")

    for jname, job in jobs.items():
        where = f"jobs.{jname}"
        if not isinstance(job, dict):
            lint.err(where, "not a mapping")
            continue
        if "uses" not in job:
            if "runs-on" not in job:
                lint.err(where, "no runs-on")
            if not isinstance(job.get("steps"), list):
                lint.err(where, "no steps")
        perm = job.get("permissions")
        if top_perm is None and perm is None:
            lint.err(where, "no permissions (set them at the top level or on the job)")
        if perm == "write-all":
            lint.err(f"{where}.permissions", "write-all is not allowed")
        if isinstance(perm, dict) and perm.get("contents") == "write" and jname not in allowed_write:
            lint.err(f"{where}.permissions", "contents: write on a job that is not allow-listed as publishing")
        needs = job.get("needs", [])
        needs = [needs] if isinstance(needs, str) else list(needs or [])
        for n in needs:
            if n not in jobs:
                lint.err(f"{where}.needs", f"unknown job {n!r}")
        step_ids = {s.get("id") for s in job.get("steps", []) if isinstance(s, dict) and s.get("id")}
        for p, key, val in walk(job, where):
            for m in re.finditer(r"\bneeds\.([\w-]+)\.outputs\.([\w-]+)", val):
                dep = m.group(1)
                if dep not in needs:
                    lint.err(p, f"needs.{dep} used but {dep!r} is not in needs:")
                elif m.group(2) not in (jobs.get(dep, {}).get("outputs") or {}):
                    lint.err(p, f"job {dep!r} declares no output {m.group(2)!r}")
            for m in re.finditer(r"\bsteps\.([\w-]+)\.outputs\b", val):
                if m.group(1) not in step_ids:
                    lint.err(p, f"no step with id {m.group(1)!r} in this job")
        for i, step in enumerate(job.get("steps", []) or []):
            if not isinstance(step, dict):
                continue
            uses = step.get("uses")
            if uses and not uses.startswith("./") and not uses.startswith("docker://"):
                if not SHA_PIN.match(uses):
                    lint.err(f"{where}.steps[{i}]", f"`uses: {uses}` is not pinned to a 40-hex commit SHA")
            if "run" in step and "uses" in step:
                lint.err(f"{where}.steps[{i}]", "step has both run and uses")

    for p, key, val in walk(doc):
        in_env = ".env." in f"{p}." or p.endswith(".env") or p.startswith("env.")
        for m in EXPR.finditer(val):
            expr = m.group(1)
            check_expr(lint, p, expr)
            if "secrets." in expr and not in_env:
                lint.err(p, "secrets may only be passed through env: (by name), never inline")
            if key == "run" or p.endswith(".run"):
                for pat in UNSAFE_IN_RUN:
                    if pat.search(expr):
                        lint.err(p, f"script injection risk: ${{{{{expr.strip()}}}}} in run:; pass it through env:")
        if val.count("${{") != val.count("}}") and "${{" in val:
            lint.err(p, "unbalanced ${{ }}")
    return lint.errors


def main(argv: list[str]) -> int:
    files = [pathlib.Path(a) for a in argv] or sorted(WORKFLOWS.glob("*.yml")) + sorted(WORKFLOWS.glob("*.yaml"))
    if not files:
        print("lint-workflows: no workflow files")
        return 1
    errors: list[str] = []
    for f in files:
        errs = lint_file(f)
        errors += errs
        print(f"{'FAIL' if errs else 'ok  '} {f.name}")
    for e in errors:
        print(f"  {e}")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
