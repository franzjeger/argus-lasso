#!/usr/bin/env python3
"""Summarize per-frame MangoHud logs, excluding the first logging second."""
import csv, json, statistics, sys
from pathlib import Path
root=Path(sys.argv[1] if len(sys.argv)>1 else 'diagnostics/benchmark-v2')
results=[]
for directory in sorted(root.glob('[123]-*')):
    for path in directory.glob('vkcube*.csv'):
        if path.name.endswith('_summary.csv'): continue
        with path.open() as f:
            next(f);next(f)
            rows=[r for r in csv.DictReader(f) if float(r['elapsed'])>=1_000_000_000]
        if not rows: continue
        span_ms=(max(float(r['elapsed']) for r in rows)-min(float(r['elapsed']) for r in rows))/1e6
        # A single interval longer than the entire capture is corrupt source
        # data. Reject the run; never silently trim a real frametime spike.
        if any(float(r['frametime']) > span_ms + 1000 for r in rows):
            print(f'Rejected corrupt frametime data: {path}', file=sys.stderr)
            continue
        times=[float(r['frametime']) for r in rows]
        # Exclude invalid values rather than dividing by zero.
        times=[t for t in times if t>0]
        slow=sorted(times,reverse=True)[:max(1,(len(times)+99)//100)]
        cpu_path=directory/'cpu.json'
        cpu=json.loads(cpu_path.read_text()) if cpu_path.exists() else {}
        result=dict(run=directory.name,frames=len(times),avg_fps=1000/statistics.mean(times),mean_ms=statistics.mean(times),low_1_fps=1000/statistics.mean(slow),gpu_load_mean=statistics.mean(float(r['gpu_load']) for r in rows),**cpu)
        results.append(result)
print(json.dumps(results,indent=2))
(root/'results.json').write_text(json.dumps(results,indent=2)+'\n')
print('\nMedian by case:')
for case in ['off','loaded','hud','legacy']:
    rs=[r for r in results if r['run'].endswith('-'+case)]
    if rs:
        print(case, 'runs=',len(rs), 'fps=',round(statistics.median(r['avg_fps'] for r in rs),1), 'ms=',round(statistics.median(r['mean_ms'] for r in rs),4), 'CPU % of one core=',round(statistics.median(r.get('one_core_percent',0) for r in rs),1))
