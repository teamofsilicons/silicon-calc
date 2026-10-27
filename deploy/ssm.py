#!/usr/bin/env python3
"""Dispatch secret-free deployment scripts, with receipts and bounded polling."""
import argparse
import json
import pathlib
import subprocess
import time

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('instance')
p.add_argument('script', type=pathlib.Path)
p.add_argument('--timeout', type=int, default=3600)
p.add_argument('--region', default='us-west-2')
p.add_argument('--dispatch-only', action='store_true')
a = p.parse_args()

def aws(*args):
    result = subprocess.run(['aws', '--profile', 'silicon-production', '--region', a.region, '--no-cli-pager', *args], capture_output=True, text=True, check=True)
    return json.loads(result.stdout)

response = aws('ssm', 'send-command', '--document-name', 'AWS-RunShellScript', '--instance-ids', a.instance,
               '--parameters', json.dumps({'commands': [a.script.read_text()], 'executionTimeout': [str(a.timeout)]}),
               '--comment', 'Silicon Calc deployment')
command = response['Command']['CommandId']
print(json.dumps({'command_id': command, 'instance_id': a.instance}), flush=True)
if a.dispatch_only:
    raise SystemExit(0)
deadline = time.monotonic() + a.timeout + 30
while time.monotonic() < deadline:
    time.sleep(5)
    try:
        result = aws('ssm', 'get-command-invocation', '--command-id', command, '--instance-id', a.instance)
    except subprocess.CalledProcessError as error:
        if 'InvocationDoesNotExist' in error.stderr:
            continue
        raise
    if result['Status'] in ('Pending', 'InProgress', 'Delayed'):
        continue
    print(json.dumps({key: result.get(key) for key in ['Status', 'ResponseCode', 'StandardOutputContent', 'StandardErrorContent']}, indent=2))
    raise SystemExit(0 if result['Status'] == 'Success' else 1)
raise SystemExit('SSM command did not finish before its timeout')
