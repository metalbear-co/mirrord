"""Prepares the cluster that an end-to-end test target runs on, and then runs the target's tests.

`cluster_test` runs this program with each end-to-end target's cluster and runtime as its
arguments: the cluster that the target names, and the runtime, kind or minikube, that
`--//tests:cluster_runtime` picks. The target's files, which are its tests, its images, its
setup program and the kind, minikube, kubectl and helm that Bazel downloads, come in
`CLUSTER_TEST_SETTINGS`, by their paths in the runfiles. The program creates the cluster if it
doesn't exist, checks that the cluster can run the tests' pods, installs Argo Rollouts in it if the
target's tests need it, writes its kubeconfig to the test's temporary directory, loads the images
that Bazel built and runs the setup program. It then runs the tests with that kubeconfig, and with
the variables that the setup program wrote to the file in `CLUSTER_TEST_ENVIRONMENT_FILE`. When the
tests fail, it keeps the logs of mirrord's proxies in the test's undeclared outputs.

With the runtime `none`, it only runs the tests, against the cluster in `KUBECONFIG`.
"""

import argparse
import json
import os
import pwd
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import uuid
from abc import ABC, abstractmethod
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from mirrord_tools import rlocation, run, runfile

ARGO_ROLLOUTS_MANIFEST = "https://github.com/argoproj/argo-rollouts/releases/download/v1.9.1/install.yaml"

# The image that the pull check pulls. Most tests run it, so the node usually has its layers, and
# the check only asks the registry for the manifest.
PULL_CHECK_IMAGE = "ghcr.io/metalbear-co/mirrord-pytest:latest"


def require(command, environment):
    if not run(command, env=environment):
        sys.exit(f"`{' '.join(map(str, command))}` failed.")


def succeeds(command, environment):
    """Runs a command that only answers a question, like whether a cluster exists, by its exit code,
    so its output would only look like an error."""
    with open(os.devnull, "w") as devnull:
        return run(command, output=devnull, env=environment)


class ClusterRuntime(ABC):
    """A program that runs the tests' clusters on this machine, like kind or minikube.

    Its commands run with `environment`, whose `KUBECONFIG` is where the runtime writes the cluster.
    """

    # The runtime's program, which `--//tests:cluster_runtime` names too.
    program: str

    def __init__(self, cluster, environment):
        self.cluster = cluster
        self.environment = environment

    @abstractmethod
    def reuse(self):
        """Writes the cluster to `KUBECONFIG` if it exists and runs, and returns whether it did."""

    @abstractmethod
    def create(self):
        """Creates the cluster, or starts it if it exists but doesn't run, and writes it to
        `KUBECONFIG`."""

    @abstractmethod
    def load_image(self, archive):
        """Loads the image in the archive `archive` into the cluster's nodes."""

    @abstractmethod
    def delete_command(self):
        """Returns the command that deletes the cluster, for a developer to run."""

    def prepare(self):
        if self.reuse():
            print(f"Reusing the {self.program} cluster {self.cluster}.", flush=True)
        else:
            self.create()
            print(
                f"Created the {self.program} cluster {self.cluster}. Delete it with `{self.delete_command()}`.",
                flush=True,
            )

    def require(self, *arguments):
        require([self.program, *arguments], self.environment)

    def succeeds(self, *arguments):
        return succeeds([self.program, *arguments], self.environment)


class Kind(ClusterRuntime):
    program = "kind"

    def __init__(self, cluster, environment, ipv6):
        super().__init__(cluster, environment)
        self.config = ["--config", runfile("tests/kind-cluster-ipv6-config.yaml")] if ipv6 else []

    def reuse(self):
        # `kind export kubeconfig` fails when the cluster doesn't exist.
        return self.succeeds("export", "kubeconfig", "--name", self.cluster)

    def create(self):
        self.require("create", "cluster", "--name", self.cluster, "--wait", "120s", *self.config)

    def load_image(self, archive):
        self.require("load", "image-archive", archive, "--name", self.cluster)

    def delete_command(self):
        return f"bazel run @multitool//tools/kind -- delete cluster --name {self.cluster}"


class Minikube(ClusterRuntime):
    program = "minikube"

    def reuse(self):
        # `minikube update-context` fails when the cluster doesn't exist, and `minikube status` when
        # the cluster doesn't run.
        return self.succeeds("update-context", "--profile", self.cluster) and self.succeeds(
            "status", "--profile", self.cluster
        )

    def create(self):
        # This also starts a stopped cluster, or one whose last start failed. containerd matches
        # kind's nodes. minikube limits its node to 2 CPUs and a few GB of memory by default, which
        # the tests, running several at a time, outgrow, so the node gets the whole machine, as
        # kind's does.
        self.require(
            "start",
            "--profile",
            self.cluster,
            "--container-runtime",
            "containerd",
            "--cpus",
            "max",
            "--memory",
            "max",
            "--wait",
            "all",
        )

    def load_image(self, archive):
        self.require("image", "load", archive, "--profile", self.cluster)

    def delete_command(self):
        return f"bazel run @multitool//tools/minikube -- delete --profile {self.cluster}"


def check_image_pull(environment):
    """Returns `None` if the cluster's node can pull `PULL_CHECK_IMAGE` from its registry, or the
    reason it can't.

    A node can be Ready while it can't reach any registry, for example a kind node whose Docker
    network lost its DNS when the machine restarted. Every test would then wait for its pods until
    it times out, so the check fails as soon as the kubelet reports the failed pull.
    """
    pod = f"mirrord-e2e-pull-check-{uuid.uuid4().hex[:8]}"
    kubectl = ["kubectl", "--namespace", "default"]
    require(
        [*kubectl, "run", pod, "--image", PULL_CHECK_IMAGE, "--image-pull-policy=Always", "--restart=Never"],
        environment,
    )
    try:
        deadline = time.monotonic() + 300
        while time.monotonic() < deadline:
            status = json.loads(
                subprocess.run(
                    [*kubectl, "get", "pod", pod, "--output", "json"],
                    env=environment,
                    capture_output=True,
                    check=True,
                ).stdout
            )["status"]
            for container in status.get("containerStatuses", []):
                state = container["state"]
                if "running" in state or "terminated" in state:
                    return None
                waiting = state.get("waiting", {})
                if waiting.get("reason") in ["ErrImagePull", "ImagePullBackOff"]:
                    return waiting.get("message", waiting["reason"])
            time.sleep(2)
        return "the pod didn't start in 5 minutes"
    finally:
        succeeds([*kubectl, "delete", "pod", pod, "--wait=false"], environment)


def prepare_cluster(runtime_name, cluster, ipv6, argo_rollouts, image_archives):
    """Prepares the target's cluster, installs Argo Rollouts in it if `argo_rollouts` is set, loads
    the images in `image_archives` into it, and returns the path of the cluster's kubeconfig."""
    # Bazel downloads the other programs, but kind and minikube run the cluster's nodes as Docker
    # containers.
    if shutil.which("docker") is None:
        sys.exit("The end-to-end tests need `docker`, which isn't on PATH.")

    # CI's IPv6 job turns on IPv6 forwarding before the tests. Doing that here would need root, so
    # this only warns. On macOS, the containers run in the virtual machine of Docker Desktop or of
    # a similar tool, whose settings this machine doesn't show.
    forwarding = Path("/proc/sys/net/ipv6/conf/all/forwarding")
    if ipv6 and sys.platform == "linux" and forwarding.read_text().strip() != "1":
        print(
            "Warning: IPv6 forwarding is off. CI turns it on for the IPv6 tests, with "
            "`sudo sysctl -w net.ipv6.conf.all.forwarding=1`.",
            flush=True,
        )

    kubeconfig = Path(os.environ["TEST_TMPDIR"], "kubeconfig")

    # Bazel gives each test a `HOME` of its own, where the Docker CLI wouldn't find its contexts in
    # `~/.docker`, and minikube wouldn't find its profiles in `~/.minikube`. So the cluster tools run
    # with the user's real home.
    environment = {
        **os.environ,
        "HOME": pwd.getpwuid(os.getuid()).pw_dir,
        "KUBECONFIG": str(kubeconfig),
    }

    runtime = Kind(cluster, environment, ipv6) if runtime_name == "kind" else Minikube(cluster, environment)
    runtime.prepare()

    # A cluster that survived a restart of the machine or of Docker can look fine while its
    # network is broken, so every run checks the cluster before it runs the tests.
    def broken(problem):
        sys.exit(
            f"The {runtime.program} cluster {cluster} is broken: {problem}\nThe tests would only time "
            f"out, so delete the cluster with `{runtime.delete_command()}`, and the next run creates it again."
        )

    wait = ["kubectl", "wait", "--for=condition=Ready", "--timeout=120s"]
    if not run([*wait, "nodes", "--all"], env=environment):
        broken("its node isn't ready.")
    if not run([*wait, "pods", "--all", "--namespace", "kube-system"], env=environment):
        broken("its system pods, like CoreDNS, aren't ready.")
    if reason := check_image_pull(environment):
        broken(f"its node can't pull {PULL_CHECK_IMAGE}: {reason}")

    # Argo Rollouts' manifest creates the Deployment last, so checking for it also catches an install
    # that failed halfway.
    if argo_rollouts and not succeeds(
        ["kubectl", "get", "deployment", "argo-rollouts", "--namespace", "argo-rollouts"], environment
    ):
        if not succeeds(["kubectl", "get", "namespace", "argo-rollouts"], environment):
            require(["kubectl", "create", "namespace", "argo-rollouts"], environment)
        require(["kubectl", "apply", "--namespace", "argo-rollouts", "-f", ARGO_ROLLOUTS_MANIFEST], environment)

    # A failed load exits its thread, and reading the results raises that exit here.
    with ThreadPoolExecutor() as pool:
        list(pool.map(runtime.load_image, image_archives))

    return kubeconfig


def main():
    parser = argparse.ArgumentParser(
        description="Prepares the cluster of an end-to-end test target, and runs the target's tests on it."
    )
    parser.add_argument("--runtime", choices=["kind", "minikube", "none"], required=True)
    parser.add_argument("--cluster", required=True, help="the kind cluster or the minikube profile")
    parser.add_argument("--ipv6", action="store_true", help="create an IPv6-only kind cluster")
    parser.add_argument("--argo-rollouts", action="store_true", help="install Argo Rollouts in the cluster")
    parser.add_argument("test_args", nargs="*", help="the arguments of the tests, after `--`")
    args = parser.parse_args()

    # The tests get the environment that Bazel gave this program, without the variables that
    # rules_python's bootstrap added. `PYTHONSAFEPATH=1` would keep the tests' Python apps from
    # importing their own modules, and the runfiles root would point other rules_python programs
    # at this program's runfiles.
    test_environment = {
        name: value
        for name, value in os.environ.items()
        if name not in ["PYTHONSAFEPATH", "RULES_PYTHON_TESTING_RUNFILES_ROOT"]
    }
    settings = json.loads(test_environment.pop("CLUSTER_TEST_SETTINGS"))

    # kind, minikube, kubectl and helm come from the runfiles, and this program, the tests and the
    # setup program run them by name.
    tool_directories = dict.fromkeys(str(Path(rlocation(tool)).parent) for tool in settings["tools"])
    test_environment["PATH"] = os.environ["PATH"] = os.pathsep.join([*tool_directories, os.environ["PATH"]])

    if args.runtime == "none":
        # This program loads no image into a cluster that it didn't prepare, and an image that
        # Bazel built may be tagged with its digest, as the agent's is, so the developer names the
        # images that the cluster has.
        for missing in ["KUBECONFIG", *settings["image_variables"]]:
            if missing not in test_environment:
                options = " ".join(
                    [
                        "--test_env=KUBECONFIG=<path>",
                        *(f"--test_env={variable}=<image>" for variable in settings["image_variables"]),
                    ]
                )
                sys.exit(
                    f"With the cluster runtime none, pass the cluster and the images with {options}. "
                    f"{missing} is missing."
                )
    else:
        # The tests run another image, like a released agent, when the developer names it in the
        # image's variable, and the cluster pulls that image.
        image_archives = {
            variable: rlocation(archive)
            for variable, archive in settings["image_archives"].items()
            if variable not in test_environment
        }
        test_environment["KUBECONFIG"] = str(
            prepare_cluster(args.runtime, args.cluster, args.ipv6, args.argo_rollouts, image_archives.values())
        )

        # The tests read each image's name from its variable, as the mirrord CLI that they start
        # reads the agent's from `MIRRORD_AGENT_IMAGE`.
        for variable, path in image_archives.items():
            # The archive's `manifest.json` names the image that it holds, as `docker save` does.
            with tarfile.open(path) as archive:
                manifest_file = archive.extractfile("manifest.json")
                if manifest_file is None:
                    sys.exit(f"`manifest.json` in {path} isn't a file.")
                manifest = json.load(manifest_file)
            test_environment[variable] = manifest[0]["RepoTags"][0]

        if "setup" in settings:
            program, *setup_args = settings["setup"]["command"]
            # The setup program can pass the tests what it found out, like the kubeconfig of a user
            # that it created, which only exists once it ran.
            environment_file = Path(os.environ["TEST_TMPDIR"], "cluster-test-environment.json")
            require(
                [rlocation(program), *setup_args],
                {
                    **test_environment,
                    **settings["setup"]["environment"],
                    "CLUSTER_TEST_ENVIRONMENT_FILE": str(environment_file),
                },
            )
            if environment_file.exists():
                test_environment.update(json.loads(environment_file.read_text()))

    # The mirrord CLIs that the tests start write the intproxy's and the extproxy's logs to their
    # temporary directory, so the tests get one of their own, and a failed run keeps the logs among
    # the test's outputs, which CI uploads. The directory isn't in `TEST_TMPDIR`, whose path is too
    # long for the Unix sockets that the intproxy creates in it.
    with tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as temporary_directory:
        test_environment["TMPDIR"] = temporary_directory
        if not run([rlocation(settings["tests"]), *args.test_args], env=test_environment):
            for log in Path(temporary_directory).glob("mirrord-*proxy-*.log"):
                shutil.copy(log, os.environ["TEST_UNDECLARED_OUTPUTS_DIR"])
            sys.exit(1)


if __name__ == "__main__":
    main()
