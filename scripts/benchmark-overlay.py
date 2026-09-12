#!/usr/bin/env python3
"""Controlled native-Vulkan microbenchmark; not a PoE2 gameplay benchmark.
MangoHud remains loaded with no HUD in every case, capturing every frame.
"""
import argparse, json, os, socket, subprocess, time
from pathlib import Path
p=argparse.ArgumentParser()
p.add_argument('--repeats',type=int,default=3)
p.add_argument('--duration',type=int,default=10)
p.add_argument('--output',type=Path,default=Path('diagnostics/benchmark'))
p.add_argument('--legacy-library',type=Path)
p.add_argument('--cases',nargs='+',choices=['off','loaded','hud'],default=['off','loaded','hud'])
a=p.parse_args();a.output.mkdir(parents=True,exist_ok=True)
cases=list(a.cases)
if a.legacy_library:
    layerdir=(a.output/'legacy-manifest').resolve();layerdir.mkdir(exist_ok=True)
    (layerdir/'ArgusLegacy.json').write_text(json.dumps({'file_format_version':'1.0.0','layer':{'name':'VK_LAYER_ARGUS_LEGACY','type':'GLOBAL','library_path':str(a.legacy_library.resolve()),'api_version':'1.3.200','implementation_version':'1','description':'Archived Argus baseline'}}))
    cases+=['legacy']
for repeat in range(a.repeats):
    # Rotate order across repetitions to reduce order and warmup bias.
    order=cases[repeat:]+cases[:repeat]
    for case in order:
        folder=(a.output/f'{repeat+1}-{case}').resolve();folder.mkdir(exist_ok=True)
        env=os.environ.copy()
        for key in ['ARGUS_LASSO_HUD','ARGUS_LASSO_HUD_DISABLE','ARGUS_LASSO_DRAW','VK_INSTANCE_LAYERS','VK_ADD_LAYER_PATH','VK_LAYER_VALIDATE_SYNC']:
            env.pop(key,None)
        env['MANGOHUD_CONFIG']=f'no_display,control=argus-bench-%p,log_interval=0,output_folder={folder},permit_upload=0'
        if case=='off':env['ARGUS_LASSO_HUD_DISABLE']='1'
        elif case=='legacy':env.update(VK_ADD_LAYER_PATH=str(layerdir),VK_INSTANCE_LAYERS='VK_LAYER_ARGUS_LEGACY')
        else:env.update(ARGUS_LASSO_HUD='1',ARGUS_LASSO_DRAW='0' if case=='loaded' else '1')
        command=['mangohud','vkcube','--wsi','wayland','--present_mode','0','--width','1280','--height','720']
        with (folder/'process.log').open('w') as log:
            proc=subprocess.Popen(command,env=env,stdout=log,stderr=log)
            time.sleep(5)
            # MangoHud 0.8.4 skips autostart while no_display is set. Start
            # explicitly using its per-process abstract control socket.
            control=socket.socket(socket.AF_UNIX);control.settimeout(3)
            control.connect('\0argus-bench-'+str(proc.pid))
            control.sendall(b':logging=1;')
            time.sleep(1)
            def cpu_seconds():
                fields=Path(f'/proc/{proc.pid}/stat').read_text().rsplit(')',1)[1].split()
                return (int(fields[11])+int(fields[12]))/os.sysconf('SC_CLK_TCK')
            before=cpu_seconds();started=time.monotonic()
            time.sleep(a.duration)
            seconds=time.monotonic()-started;cpu=cpu_seconds()-before
            (folder/'cpu.json').write_text(json.dumps({'duration_s':seconds,'process_cpu_s':cpu,'one_core_percent':100*cpu/seconds}))
            control.sendall(b':logging=0;');time.sleep(1);control.close()
            maps=Path(f'/proc/{proc.pid}/maps')
            if maps.exists():
                (folder/'mapped-layers.txt').write_text('\n'.join(l for l in maps.read_text().splitlines() if 'argus' in l or 'MangoHud' in l))
            proc.terminate()
            try:proc.wait(timeout=5)
            except subprocess.TimeoutExpired:proc.kill();proc.wait()
        if not list(folder.glob('*.csv')): raise RuntimeError(f'No benchmark data in {folder}')
        print(f'Completed repetition {repeat+1}: {case}',flush=True)
