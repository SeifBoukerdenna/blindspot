#!/usr/bin/python3
import json
import os
import pathlib
import sys
import time

root = pathlib.Path(__file__).parent
args = sys.argv[1:]
if any(key in os.environ for key in ('DOCKER_HOST', 'DOCKER_CONTEXT', 'CONTAINER_HOST', 'CONTAINER_CONNECTION', 'DOCKER_CONFIG')):
    sys.exit(8)
if args == ['huge']:
    sys.stdout.write('x' * 600000)
    sys.exit(0)
if args == ['slow']:
    time.sleep(4)
    sys.exit(0)
if args == ['failure']:
    sys.exit(3)
with (root / 'calls.jsonl').open('a') as f:
    f.write(json.dumps(args) + '\n')
if args[:2] == ['--remote', '--url']:
    args = args[3:]
elif args[:1] == ['--host']:
    args = args[2:]
else:
    sys.exit(4)
state = (root / 'state').read_text() if (root / 'state').exists() else 'running'
cid = 'a' * 64
if args[0] == 'ps':
    if state != 'missing':
        print(json.dumps(dict(id=cid, name='harbor-api', image='harbor/api:dev', state=state,
                              status='Up 12 minutes' if state == 'running' else 'Exited (0)',
                              ports='0.0.0.0:8080->80/tcp')))
elif args[0] == 'logs':
    print('\033[32mReady on port 80\033[0m')
    print('sample error stream', file=sys.stderr)
    if '--follow' in args:
        sys.stdout.flush()
        (root / 'stream.pid').write_text(str(os.getpid()))
        time.sleep(30)
elif args[:2] == ['image', 'ls']:
    if not (root / 'no-image').exists():
        for tag in ['dev', 'stable']:
            print(json.dumps(dict(id='sha256:' + 'b' * 64, repository='harbor/api', tag=tag,
                                  size='42 MB', created='2026-08-12')))
        print(json.dumps(dict(id='c' * 64, repository='<none>', tag='<none>', size='12 MB', created='2026-08-10')))
elif args[:2] == ['container', 'inspect'] and args[-1] == cid:
    print(json.dumps([dict(Id=cid, Created='2026-08-12', Config={'Env': ['SECRET=not-for-display']},
                          State={'Health': {'Status': 'healthy'}},
                          HostConfig={'RestartPolicy': {'Name': 'unless-stopped'}},
                          Mounts=[{'Source': '/fixture/data', 'Destination': '/data'}],
                          NetworkSettings={'Networks': {'bridge': {}}, 'Ports': {
                              '80/tcp': [{'HostIp': '0.0.0.0', 'HostPort': '8080'}],
                              '81/tcp': [{'HostIp': '198.51.100.10', 'HostPort': '9090'}],
                              '53/udp': [{'HostIp': '127.0.0.1', 'HostPort': '5353'}]}})]))
elif args[0] == 'stats' and args[-1] == cid and '--no-stream' in args:
    if args[args.index('--format') + 1].startswith('{'):
        print(json.dumps(dict(cpu='0.2%', memory='20MiB / 1GiB', network='1kB / 2kB', disk='0B / 0B')))
    else:
        print('CPU: 0.2% · Memory: 20MiB / 1GiB · Network: 1kB / 2kB · Disk I/O: 0B / 0B')
elif args[0] == 'events':
    print(json.dumps(dict(Actor={'ID': cid}, Action='start', time=int(time.time()))), flush=True)
    time.sleep(30)
elif args[:2] == ['volume', 'inspect'] and args[-1] == 'fixture_data':
    print(json.dumps([{'Name': 'fixture_data'}]))
elif args[0] == 'run' and args[-1] == 'b' * 64 and args[2:4] == ['--pull', 'never']:
    if '--env-file' in args:
        env = pathlib.Path(args[args.index('--env-file') + 1])
        assert env.stat().st_mode & 0o777 == 0o600
        assert env.parent.stat().st_mode & 0o777 == 0o700
        assert env.read_text() == 'MODE=override\nTOKEN=fixture-secret'
        (root / 'env-result').write_text('validated')
        if (root / 'fail-create').exists():
            sys.exit(9)
    (root / 'state').write_text('running')
    print(cid)
elif args[0] in ('start', 'stop', 'restart') and args[-1] == cid:
    (root / 'state').write_text('exited' if args[0] == 'stop' else 'running')
    print(cid)
else:
    sys.exit(5)
