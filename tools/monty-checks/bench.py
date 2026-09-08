"""Small, reproducible HTTP comparison; Python stdlib only. No remote deployment.

python tools/monty-checks/bench.py target/lab/celld --output results.json

Two runtimes share one binary: native Monty and an ordinary TypeScript worker. Timed
requests use keep-alive, validate every response, and report client CPU.
"""
import argparse
import asyncio
from concurrent.futures import ProcessPoolExecutor
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import socket
import subprocess
import tempfile
import time

HERE = Path(__file__).resolve().parent


async def load(port, case, concurrency, seconds, key):
    latencies, values = [], []
    expected = {"hello": b"Hello, world!", "echo": json.dumps({"value":"x"*16384},separators=(',',':')).encode(), "pause":b"done"}
    expected.update(object=b'{"value":1}', fetch=expected["echo"])
    args = {"echo":{"value":"x"*16384}, "increment":{"id":key}, "object":{"id":key}, "fetch":{"url":f"http://127.0.0.1:{port}/echo"}}.get(case,{})
    body = json.dumps(args,separators=(',',':')).encode()
    payload = f"POST /{case} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {len(body)}\r\nConnection: keep-alive\r\n\r\n".encode()+body
    connections = [await asyncio.open_connection('127.0.0.1',port,limit=128*1024) for _ in range(concurrency)]

    async def one(reader, writer, deadline, record):
        previous = 0
        while time.monotonic() < deadline:
            before = time.perf_counter()
            writer.write(payload)
            await writer.drain()
            head = await asyncio.wait_for(reader.readuntil(b'\r\n\r\n'),10)
            lines = head.decode('latin1').split('\r\n')
            headers = dict(line.lower().split(': ',1) for line in lines[1:] if ': ' in line)
            if 'content-length' in headers:
                response = await reader.readexactly(int(headers['content-length']))
            else:
                assert headers.get('transfer-encoding') == 'chunked', head
                chunks=[]
                while True:
                    size=int((await reader.readline()).split(b';')[0],16)
                    if not size:
                        assert await reader.readline() == b'\r\n'
                        break
                    chunks.append(await reader.readexactly(size))
                    assert await reader.readexactly(2) == b'\r\n'
                response=b''.join(chunks)
            assert lines[0].split()[1] == '200', (case, lines[0], response[:1000])
            if case == 'increment':
                value=json.loads(response)['value']
                assert isinstance(value,int) and value>previous, response
                previous=value
                if record: values.append(value)
            else:
                assert response == expected[case], response[:200]
            if record: latencies.append((time.perf_counter()-before)*1000)

    try:
        warm_until=time.monotonic()+.25
        await asyncio.gather(*(one(r,w,warm_until,False) for r,w in connections))
        cpu=time.process_time()
        start=time.monotonic()
        await asyncio.gather(*(one(r,w,start+seconds,True) for r,w in connections))
        end=time.monotonic()
        return {"start":start,"end":end,"cpu_s":time.process_time()-cpu,"latencies_ms":latencies,"values":values}
    finally:
        for _,writer in connections: writer.close()
        await asyncio.gather(*(w.wait_closed() for _,w in connections))


def client(*args):
    return asyncio.run(load(*args))


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary',type=Path)
    parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--include-fetch', action='store_true', help='requires a custom host with fetch middleware')
    parser.add_argument('--seconds',type=float,default=2)
    parser.add_argument('--repeats',type=int,default=3)
    parser.add_argument('--concurrency',type=int,nargs='+',default=[1,16])
    options=parser.parse_args()
    binary=options.binary.resolve()
    assert options.seconds>0 and options.repeats>0 and all(c>0 for c in options.concurrency)
    results={"timestamp":datetime.now(timezone.utc).isoformat(),"binary_sha256":hashlib.sha256(binary.read_bytes()).hexdigest(),
             "platform":platform.platform(),"cpu_affinity":len(os.sched_getaffinity(0)),
             "cpu_quota":Path('/sys/fs/cgroup/cpu.max').read_text().strip(),
             "seconds":options.seconds,"repeats":options.repeats,"isolates":1,"samples":[]}
    options.output.parent.mkdir(parents=True,exist_ok=True)
    with tempfile.TemporaryDirectory() as directory, ProcessPoolExecutor(max_workers=4) as pool:
        root=Path(directory)
        source=(HERE/'bench/worker.py').read_text()
        for runtime in ['monty','typescript']:
            project=root/runtime; project.mkdir()
            config={"name":"monty-http-bench","main":"worker.py"}
            if runtime=='monty': (project/'worker.py').write_text(source)
            else:
                config.update(main='worker.ts',durable_objects={"bindings":[{"name":"COUNTER","class_name":"Counter"}]},migrations=[{"tag":"v1","new_sqlite_classes":["Counter"]}])
                shutil.copyfile(HERE/'bench/worker.ts',project/'worker.ts')
            (project/'wrangler.jsonc').write_text(json.dumps(config))
        for repeat in range(options.repeats):
            order=['monty','typescript']
            order=order[repeat%2:]+order[:repeat%2]
            for runtime in order:
                with socket.socket() as sock:
                    sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
                env={k:v for k,v in os.environ.items() if not k.startswith('CELLD_')}
                env.update(CELLD_MAX_STATELESS_ISOLATES='1',RUST_LOG='error')
                with (root/f'{runtime}.log').open('w') as log:
                    process=subprocess.Popen([str(binary),'dev',str(root/runtime),'--port',str(port),'--logs'],env=env,stdout=log,stderr=log)
                    try:
                        deadline=time.monotonic()+120
                        while time.monotonic()<deadline:
                            if process.poll() is not None: raise RuntimeError((root/f'{runtime}.log').read_text())
                            try:
                                with socket.create_connection(('127.0.0.1',port),timeout=.1): break
                            except OSError: time.sleep(.1)
                        else: raise RuntimeError('server startup timeout')
                        for concurrency in options.concurrency:
                            for case in ['hello','echo','object','increment','pause'] + (['fetch'] if options.include_fetch else []):
                                clients=min(4,concurrency)
                                sizes=[concurrency//clients+(i<concurrency%clients) for i in range(clients)]
                                jobs=[pool.submit(client,port,case,n,options.seconds,f'counter-{repeat}-{concurrency}') for n in sizes]
                                parts=[job.result(timeout=options.seconds+45) for job in jobs]
                                elapsed=max(p['end'] for p in parts)-min(p['start'] for p in parts)
                                latency=sorted(v for p in parts for v in p['latencies_ms'])
                                values=[v for p in parts for v in p['values']]
                                assert len(values)==len(set(values)), 'duplicate durable updates'
                                sample={"runtime":runtime,"repeat":repeat,"case":case,"concurrency":concurrency,
                                        "requests":len(latency),"seconds":elapsed,"rps":len(latency)/elapsed,
                                        "p50_ms":latency[int(len(latency)*.5)],"p95_ms":latency[int(len(latency)*.95)],
                                        "client_cpu_cores":sum(p['cpu_s'] for p in parts)/elapsed,"errors":0}
                                results['samples'].append(sample)
                                options.output.write_text(json.dumps(results,indent=2)+'\n')
                                print(f"{repeat+1} {runtime:10} {case:9} c={concurrency:2} {sample['rps']:8.0f} req/s p95={sample['p95_ms']:.2f}ms clientCPU={sample['client_cpu_cores']:.2f}",flush=True)
                    except BaseException:
                        print((root/f'{runtime}.log').read_text())
                        raise
                    finally:
                        process.terminate()
                        try: process.wait(timeout=15)
                        except subprocess.TimeoutExpired: process.kill();process.wait()
    print(f"Saved {len(results['samples'])} validated samples to {options.output}")


if __name__=='__main__': main()
