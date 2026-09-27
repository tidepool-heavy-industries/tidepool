"""Aggregate Jev usage from a bounded run-log prefix without retaining bodies."""
import argparse
import collections
import json
import re
from pathlib import Path
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("log", type=Path)
parser.add_argument("--lines", type=int, help="Read exactly this prefix of a live log")
parser.add_argument("--output", type=Path, required=True)
args = parser.parse_args()
p = args.log
calls=[]; slots=[]; failures=[]; last_line=0; last_time=None
for ln,line in enumerate(p.open(),1):
    if args.lines is not None and ln > args.lines:
        break
    last_line=ln; last_time=line[:27]
    ctx=re.search(r'after_tool_slot\{slot=afterTool tool=([^ ]+) actor=(\d+)@(\d+) revision=([^ ]+) ordinal=(\d+)\}',line)
    actor=re.search(r'actor=(\d+)@(\d+)',line)
    ckey=(ctx[2],ctx[3],ctx[5]) if ctx else None
    if 'tidepool_handlers::handlers::jev: jev call ' in line:
        m=re.search(r'jev call model=([^ ]+) status=(\d+) elapsed_ms=(\d+) usage=(\{.*?\})',line)
        if m:
            usage=json.loads(m[4]); calls.append(dict(line=ln,time=line[:27],model=m[1],status=int(m[2]),elapsed_ms=int(m[3]),input=usage.get('input_tokens'),output=usage.get('output_tokens'),after=bool(ctx),tool=ctx[1] if ctx else None,actor=ctx[2] if ctx else actor[1] if actor else None,key=ckey))
    if 'jev call failed' in line:
        m=re.search(r'failure=([^ ]+)',line)
        kind='max_tokens_exceeded' if 'max_tokens_exceeded' in line else ('timeout' if 'Timeout' in line else 'other')
        failures.append(dict(line=ln,time=line[:27],kind=kind,after=bool(ctx),tool=ctx[1] if ctx else None,actor=ctx[2] if ctx else actor[1] if actor else None,key=ckey))
    if 'after-tool slot invoked' in line:
        m=re.search(r'after-tool slot invoked actor=(\d+)@(\d+) tool=([^ ]+) ordinal=(\d+) elapsed_ms=(\d+) disposition=(.*?)(?: detail=|\n|$)',line)
        if m:
            disp=m[6]
            category=disp.split('(')[0]
            reason=('no_floor' if 'no heuristic crossed' in disp else 'trivial_bash' if 'trivial call' in disp else 'message_send' if 'message send:' in disp else 'transport' if 'Jev transport failed' in disp else 'other')
            slots.append(dict(line=ln,time=line[:27],actor=m[1],tool=m[3],ordinal=int(m[4]),elapsed_ms=int(m[5]),disposition=category,reason=reason,key=(m[1],m[2],m[4])))
def stats(rows):
    xs=sorted(r['input'] for r in rows if r['input'] is not None); ys=sorted(r['output'] for r in rows if r['output'] is not None)
    q=lambda z,f:z[min(len(z)-1,int(len(z)*f))] if z else None
    return dict(count=len(rows),input=sum(xs),output=sum(ys),input_min=q(xs,0),input_p50=q(xs,.5),input_p90=q(xs,.9),input_p99=q(xs,.99),input_max=q(xs,1),output_p50=q(ys,.5),output_p90=q(ys,.9),output_max=q(ys,1),elapsed_ms=sum(r['elapsed_ms'] for r in rows))
by_slot={s['key']:s for s in slots}
for c in calls:
    if c['key'] in by_slot:c['disposition']=by_slot[c['key']]['disposition'];c['reason']=by_slot[c['key']]['reason']
summary={'source':str(p),'observed_lines':last_line,'observed_last_timestamp':last_time,'successful_calls':stats(calls),'after_tool_successful':stats([c for c in calls if c['after']]),'explicit_successful':stats([c for c in calls if not c['after']]),'failed_calls':dict(total=len(failures),by_kind=dict(collections.Counter(f['kind'] for f in failures)),after_tool=sum(f['after'] for f in failures),explicit=sum(not f['after'] for f in failures)),'slots':dict(total=len(slots),by_disposition=dict(collections.Counter(s['disposition'] for s in slots)),by_reason=dict(collections.Counter(s['reason'] for s in slots)),by_tool=dict(collections.Counter(s['tool'] for s in slots))),'after_tool_by_tool':{t:stats([c for c in calls if c['after'] and c['tool']==t]) for t in sorted({c['tool'] for c in calls if c['after']})},'after_tool_by_reason':{t:stats([c for c in calls if c.get('reason')==t]) for t in sorted({c.get('reason') for c in calls if c['after']}) if t},'top_actors':[{ 'actor':a,'calls':n,'input':sum(c['input'] for c in calls if c['actor']==a)} for a,n in collections.Counter(c['actor'] for c in calls).most_common(12)],'unjoined_after_calls':sum(c['after'] and 'disposition' not in c for c in calls),'max_token_errors_by_tool':dict(collections.Counter(f['tool'] or 'explicit' for f in failures if f['kind']=='max_tokens_exceeded'))}
args.output.write_text(json.dumps(summary,indent=2)+'\n')
print(json.dumps(summary,indent=2))
