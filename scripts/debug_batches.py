import json
base = 'C:/Users/EDN/Documents/AION/project/agentgate'
for b in ['b1','b2','b3','b4','b5']:
    try:
        with open(f'{base}/batch_{b}.json') as f:
            d = json.load(f)
            print(f'{b}: {len(d)} entries, keys={list(d[0].keys()) if d else "empty"}')
    except Exception as e:
        print(f'{b}: ERROR - {e}')