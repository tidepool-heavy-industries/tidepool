#!/usr/bin/env python3
"""Reproducible bounded Wave18 rejection and non-overlapping timing inventory.

Run: python3 docs/reports/wave18-audit/audit-counts.py
Boundaries are first N rollout JSONL records (zero-based ordinals 0 through N-1); log boundary is
the fixed prefix below (this script prints count and last timestamp).
No prompt/input bodies are emitted. Rollout classification pairs tool calls/outputs; see the report for the diagnostic bucket definitions and adjudicated table. The captured host-log prefix is pinned below so the timing table is stable.
"""
import json, re, collections, pathlib, sys
if any(arg in ("-h", "--help") for arg in sys.argv[1:]):
    print("usage: python3 docs/reports/wave18-audit/audit-counts.py\nReplays the captured Wave18 rollout prefixes and fixed host-log timing prefix; emits counts and IDs only, not transcript bodies.")
    raise SystemExit(0)

RUN = pathlib.Path('/home/inanna/dev/exomonad-harness-runs/wave18')
SESS = pathlib.Path('/home/inanna/.codex/sessions/2026/09/26')
TRACES = [
 ('root', SESS/'rollout-2026-09-26T16-34-09-01a0e011-ac4e-79e0-86f6-67d10bade76f.jsonl', 2450),
 ('runtime9', SESS/'rollout-2026-09-26T16-47-39-01a0e01e-05a5-7d73-887c-4120368e3d88.jsonl', 2550),
 ('standalone10', SESS/'rollout-2026-09-26T16-47-43-01a0e01e-16cc-7852-bf82-e40dc7eb616d.jsonl', 2103),
 ('operator11', SESS/'rollout-2026-09-26T16-47-43-01a0e01e-1582-70d0-bc0c-d9be18f34d72.jsonl', 2319),
 ('reviewer13', SESS/'rollout-2026-09-26T16-48-33-01a0e01e-d94f-77d2-8d35-3a93850d4a97.jsonl', 127),
]
LOG = RUN/'.exomonad/logs/29e61b63-2bc9-45d5-b5d1-b67e20918bea.log'
LOG_PREFIX_LINES = 133731  # captured at 2026-09-27T02:19:49.312207Z

# Explicit rejection markers; precedence assigns each failed cell once: import, parse, scope/name, then type/effect. Runtime exceptions are separate.
PATS = [
 ('import/publication', re.compile(r'Could not find module|Could not load module|module .* is a member of the hidden package', re.I)),
 ('parse', re.compile(r'parse error|lexical error|possibly incorrect indentation|parse error in pattern', re.I)),
 ('scope/name', re.compile(r'Not in scope:|Variable not in scope:|Data constructor not in scope:|\bnot in scope\b', re.I)),
 ('type/effect', re.compile(r"Couldn't match (?:expected )?type|Illegal term-level use|Could not deduce|No instance for|Ambiguous type variable|Potentially matching instances|Expected:|Actual:|type mismatch|\btype error\b", re.I)),
]
RUNTIME = re.compile(r'prepared[- ](?:engine|runtime)|resident runtime exception|uncaught exception|runtime exception', re.I)

def load_prefix(path, limit):
    rows=[]
    with path.open() as f:
        for i,line in enumerate(f):
            if i>=limit: break
            try: rows.append(json.loads(line))
            except json.JSONDecodeError: continue
    return rows

print('REJECTIONS (first N JSONL records, zero-based ordinals 0..N-1)')
for name,path,limit in TRACES:
    rows=load_prefix(path,limit)
    calls={}
    for row in rows:
        p=row.get('payload',{})
        if p.get('type')=='custom_tool_call' and p.get('name')=='haskell':
            calls[p.get('call_id')]=p.get('input','')
    outs=[]
    for row in rows:
        p=row.get('payload',{})
        if p.get('type')=='custom_tool_call_output' and p.get('call_id') in calls:
            outs.append((p['call_id'],p.get('output','')))
    counts=collections.Counter(); exact=0
    for cid,out in outs:
        if not out: continue
        if RUNTIME.search(out): counts['prepared runtime exception']+=1; continue
        if not (re.search(r'<cell>.*?error:',out,re.S) or re.search(r'Could not find module|Could not load module',out,re.I)): continue
        for label,pat in PATS:
            if pat.search(out): counts[label]+=1; break
    # exact same Haskell source called later after an explicit rejected cell output
    rejected={cid for cid,out in outs if out and (RUNTIME.search(out) or any(p.search(out) for _,p in PATS))}
    seen={}
    for cid,src in calls.items():
        if cid in rejected: seen[src]=cid
        elif src in seen: exact+=1
    print(f'{name}: rollout_rows={len(rows)} captured_N={limit} haskell_calls={len(calls)} paired_outputs={len(outs)} marker_classes={dict(counts)} exact_repeat_after_rejection={exact}')

print('\nHOST LOG / TOP-LEVEL CALL TIMING')
lines=LOG.read_text(errors='replace').splitlines()[:LOG_PREFIX_LINES]
print(f'log_lines={len(lines)} fixed_prefix_lines={LOG_PREFIX_LINES} first={lines[0][:30] if lines else ""} last={lines[-1][:30] if lines else ""}')
call_re=re.compile(r'call timing tool=(\S+).*? total_ms=(\d+).*? checkout_wait_ms=(\d+).*? checkout_hold_ms=(\d+).*? compile_ms=(\d+).*? compile_count=(\d+).*? jev_ms=(\d+).*? jev_count=(\d+).*? exec_ms=(\d+)')
calls=[]
for n,s in enumerate(lines,1):
    m=call_re.search(s)
    if m:
        tool,*nums=m.groups(); total,wait,hold,compile_ms,cc,jev,jc,exec_ms=map(int,nums)
        calls.append((n,tool,total,wait,hold,compile_ms,cc,jev,jc,exec_ms,s))
slow=[x for x in calls if x[2]>10000]
print(f'parsed_call_timing={len(calls)} slow_total_gt_10000={len(slow)}')
# Mutually exclusive descriptive classification by largest observed component; component
# counts below are overlapping thresholds and must not be summed with each other or totals.
def dominant(x):
    comps={'checkout_wait':x[3], 'checkout_hold':x[4], 'compile':x[5], 'jev':x[7], 'exec':x[9]}
    best=max(comps,key=comps.get)
    return best if comps[best]>0 else 'unattributed'
print('slow_dominant_component_counts='+str(dict(collections.Counter(dominant(x) for x in slow))))
for label,idx in [('checkout_wait',3),('checkout_hold',4),('compile',5),('jev',7),('exec',9)]:
    xs=[x for x in slow if x[idx]>10000]
    print(f'slow_calls_component_gt_10000 {label}: n={len(xs)} max_ms={max((x[idx] for x in xs),default=0)} overlapping=true')
print('slow_call_examples (line, tool, actor label, total and component ms)')
for x in sorted(slow,key=lambda x:x[2],reverse=True)[:12]:
    actor=re.search(r'actor\{actor=([^} ]+)',x[10]); actor=actor.group(1) if actor else '?'
    print(f'{x[0]} {x[1]} actor={actor} total={x[2]} checkout_wait={x[3]} checkout_hold={x[4]} compile={x[5]} jev={x[7]} exec={x[9]}')

starts=[]; after=[]; phases=collections.Counter(); long_phase=[]
for n,s in enumerate(lines,1):
    if 'agent spec preparation' in s:
        m=re.search(r'actor=(\S+).*?phase="([^"]+)".*?elapsed_ms=(\d+)',s)
        if m: starts.append((n,m.group(1),m.group(2),int(m.group(3))))
    if 'after-tool slot invoked' in s:
        m=re.search(r'actor=(\S+).*?tool=(\S+).*?ordinal=(\d+).*?elapsed_ms=(\d+)',s)
        if m: after.append((n,m.group(1),m.group(2),int(m.group(4))))
    m=re.search(r'phase="([^"]+)" elapsed_ms=(\d+)',s)
    if m:
        phase,ms=m.group(1),int(m.group(2)); phases[phase]+=1
        if ms>10000: long_phase.append((n,phase,ms,s))
print(f'agent_spec_preparations={len(starts)} >10s={sum(s[3]>10000 for s in starts)} max_ms={max((s[3] for s in starts),default=0)}')
print(f'after_tool_slots={len(after)} >10s={sum(a[3]>10000 for a in after)} max_ms={max((a[3] for a in after),default=0)} (nested phase; do not add to call totals)')
print('long compiler phase counts='+str(dict(collections.Counter(p for _,p,_,_ in long_phase))))
# Show longest individual compiler phase rows; admission/response may overlap.
print('longest compile phase examples (line, phase, elapsed_ms, request id, actor/execution excerpt)')
for n,phase,ms,s in sorted(long_phase,key=lambda x:x[2],reverse=True)[:12]:
    rid=re.search(r'compile_request=([0-9a-f]+)',s); rid=rid.group(1) if rid else '?'
    actor=re.search(r'actor\{actor=([^} ]+)',s); actor=actor.group(1) if actor else '?'
    ex=re.search(r'execution="([^"]+)',s); ex=ex.group(1) if ex else '?'
    print(f'{n} {phase} {ms}ms request={rid} actor={actor} execution={ex}')
