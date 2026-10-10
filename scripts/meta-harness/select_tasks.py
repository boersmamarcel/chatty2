"""AGE-862 task selection (run once; output tasks.json). Rule is stored in tasks.json."""
import json, glob, random, os
B='/media/marcel/data/rust/swarm-results'; H='/media/marcel/data/rust/chattyapp'
allt={t['name']:t for t in json.load(open(f'{H}/harbor-chatty-dabstep-team/jobs-config/phase6-attempt2.json'))['tasks']}
lv=json.load(open(f'{B}/age-853-c2/levels.json'))
num=lambda n:int(n.split('/')[1])
hard={n for n in allt if lv.get(str(num(n)))=='hard'}
def names(p): return {t['name'] for t in json.load(open(p))['tasks']}
sub80=names(f'{H}/harbor-chatty-dabstep-team/jobs-config/team-bo3-judge-subset80.json')
ho40=names(f'{H}/harbor-chatty-dabstep-explore/jobs-config/phase7-conv-heldout40.json')
c2=json.load(open(f'{B}/age-853-c2/tasks.json')); c2all={t['name'] for t in c2['mixed']+c2['noharm']}
r1={t['name'] for t in json.load(open(f'{B}/age-853/arm-a.json'))['tasks']}
def rewards(jobdir):
    out={}
    for r in glob.glob(f'{jobdir}/*/result.json'):
        d=json.load(open(r)); vr=d.get('verifier_result') or {}
        rw=(vr.get('rewards') or {}).get('reward')
        out.setdefault(d['task_name'],[]).append(0.0 if rw is None else float(rw))
    return out
a2=rewards(f'{B}/age-853-c2/jobs/arm-a-single/arm-a-single')
a1=rewards(f'{B}/age-853/jobs/arm-a-single/arm-a-single')
print('hard',len(hard),'c2 armA trials',len(a2),'run1 armA',len(a1))
# search candidates: Hard, c2 arm-A wrong, or c2 vs run-1 arm A disagree
cand=sorted({n for n in a2 if n in hard and (min(a2[n])==0 or (n in a1 and a1[n][0]!=a2[n][0]))},key=num)
print('c2 hard scored',sum(1 for n in a2 if n in hard),'hard acc',sum(a2[n][0] for n in a2 if n in hard)/max(1,sum(1 for n in a2 if n in hard)))
print('search candidates',len(cand))
search=sorted(random.Random(8620).sample(cand,min(40,len(cand))),key=num)
excl=c2all|r1|sub80|ho40|set(search)
pool=sorted(hard-excl,key=num)
print('test pool after excluding c2 118, run-1 48, subset-80, heldout-40:',len(pool))
test=sorted(random.Random(8621).sample(pool,min(100,len(pool))),key=num)
json.dump({'search':[allt[n] for n in search],'test':[allt[n] for n in test],
 'rule':{'search':'random.Random(8620).sample(sorted Hard tasks of age-853-c2 arm A with reward 0, or whose run-1 arm-A reward disagrees with c2 arm A), 40)',
         'test':'random.Random(8621).sample(sorted(Hard 378 minus age-853-c2 118 minus age-853 run-1 48 minus subset-80 minus phase7 held-out-40 minus search), 100)',
         'search_candidates':len(cand),'test_pool':len(pool)}},open('tasks.json','w'),indent=1)
print('overlap search/test',len(set(search)&set(test)),'test/c2',len(set(test)&c2all))
