import json

base = 'C:/Users/EDN/Documents/AION/project/agentgate'
batches = {}
for b in ['b1','b2','b3','b4','b5']:
    try:
        with open(f'{base}/batch_{b}.json') as f:
            batches[b] = json.load(f)
    except:
        pass

# Normalize keys
for bid, arr in batches.items():
    for r in arr:
        if 'label' in r and 'tool_key' not in r:
            r['tool_key'] = r['label']
        if 'gateway' in r and 'env' not in r:
            r['env'] = r['gateway']

tools = ['shell','read','write','glob','grep','edit','list','execute','bash','cat','patch','task','todowrite','todoread']
exp = {'shell':'Shell','read':'Read','write':'Write','glob':'Glob','grep':'Grep','edit':'Edit','list':'List','execute':'Execute','bash':'Bash','cat':'Cat','patch':'Patch','task':'Task','todowrite':'TodoWrite','todoread':'TodoRead'}
env_models = [('b1','VPS','agg/9router/ag-poli'),('b2','VPS','agg/9router/free-forever'),('b3','LOCAL','agg/9router/ag-poli'),('b4','LOCAL','agg/9router/free-forever'),('b5','VPS','agg/opencode/deepseek-v4.1-flash-free')]

out = 'PASS/FAIL MATRIX\n'
out += '-'*110 + '\n'
out += f'{"Tool":<14} | {"Env":<6} | {"Model":<35} | {"Expected":<10} | {"Emitted":<12} | Status\n'
out += '-'*110 + '\n'
for tool in tools:
    for bid,env,model in env_models:
        if bid not in batches: continue
        r = next((x for x in batches[bid] if x['tool_key']==tool), None)
        if r:
            em = r['emitted_name'] or 'none'
            st = r['status']
            out += f'{tool:<14} | {env:<6} | {model:<35} | {exp[tool]:<10} | {em:<12} | {st}\n'

out += '\nSUMMARY PER ENV + MODEL\n'
out += '='*80 + '\n'
for bid,env,model in env_models:
    if bid not in batches: continue
    arr = batches[bid]
    p = sum(1 for x in arr if x['status']=='PASS')
    f = sum(1 for x in arr if x['status']=='FAIL')
    n = sum(1 for x in arr if x['status']=='NO_TOOL')
    e = sum(1 for x in arr if x['status'] not in ('PASS','FAIL','NO_TOOL'))
    out += f'{env:<6} | {model:<35} | PASS={p} FAIL={f} NO_TOOL={n} ERROR={e} | {p}/{len(arr)} ({(p/len(arr)*100):.1f}%)\n'

out += '\nSUMMARY PER TOOL\n'
out += '='*80 + '\n'
for tool in tools:
    all_r = []
    for bid in batches:
        r = next((x for x in batches[bid] if x['tool_key']==tool), None)
        if r: all_r.append(r)
    if not all_r: continue
    p = sum(1 for x in all_r if x['status']=='PASS')
    f = sum(1 for x in all_r if x['status']=='FAIL')
    n = sum(1 for x in all_r if x['status']=='NO_TOOL')
    e = sum(1 for x in all_r if x['status'] not in ('PASS','FAIL','NO_TOOL'))
    out += f'{tool:<14} | PASS={p} FAIL={f} NO_TOOL={n} ERROR={e} | {p}/{len(all_r)} ({(p/len(all_r)*100):.0f}%)\n'

print(out)
with open(f'{base}/TOOLING_REPORT.txt', 'w') as f:
    f.write(out)