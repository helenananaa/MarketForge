"""Build the Linux trader image from an allowlisted, secret-free context."""
import argparse
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'python'))
from marketforge.agents.sandbox import DockerSandbox


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--image', default='marketforge-agent:1')
    parser.add_argument('--codex-version', default='0.162.1')
    parser.add_argument('--opencode-version', default='1.19.0')
    args = parser.parse_args()
    sandbox = DockerSandbox(args.image)
    with tempfile.TemporaryDirectory(prefix='marketforge-runner-build-') as directory:
        context = Path(directory)
        shutil.copytree(ROOT / 'python/marketforge', context / 'python/marketforge', ignore=shutil.ignore_patterns('__pycache__', '*.pyc'))
        shutil.copy2(ROOT / 'python/pyproject.toml', context / 'python/pyproject.toml')
        shutil.copy2(ROOT / 'scripts/agent-runner/Dockerfile', context / 'Dockerfile')
        path = str(context)
        if os.name == 'nt' and sandbox.docker[0] == 'wsl':
            path = subprocess.check_output(['wsl', '-d', sandbox.docker[2], '--exec', 'wslpath', '-u', path], text=True).strip()
        subprocess.run([*sandbox.docker, 'build', '--tag', args.image,
            '--build-arg', 'CODEX_VERSION=' + args.codex_version,
            '--build-arg', 'OPENCODE_VERSION=' + args.opencode_version, path], check=True)


if __name__ == '__main__':
    main()
