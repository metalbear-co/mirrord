"""Prepares a cluster for `bazel test //mirrord/tests:e2e` (see `BUILD.bazel`), in two steps:

1. It creates the cluster with kind or minikube, or reuses it if it exists, and installs Argo
   Rollouts, which some tests target.
2. It loads the agent that the tests run into the cluster.

`//mirrord/tests:e2e_setup` does both steps. `//mirrord/tests:e2e_cluster` does only the first, and
takes the same arguments. It doesn't depend on the agent's image, so CI runs it while Bazel builds
the agent and the tests, and then loads the agent with `e2e_setup`.

The cluster is named `mirrord-e2e`, or after `--name`, and its kubeconfig is
`clusters/<runtime>/<name>`. The script asks kind or minikube whether the cluster exists, rather
than trusting the kubeconfig, which outlives a cluster that was deleted another way, and which
minikube writes before a start that fails. `--delete` deletes the cluster and the kubeconfig.

    bazel run //mirrord/tests:e2e_setup                          # creates or reuses mirrord-e2e
    bazel run //mirrord/tests:e2e_setup -- --runtime minikube    # the same with minikube
    bazel run //mirrord/tests:e2e_setup -- --name <name>         # creates or reuses <name>
    bazel run //mirrord/tests:e2e_setup -- --delete              # deletes mirrord-e2e

Arguments after a second `--` go to `kind create cluster` or `minikube start` when the script
creates the cluster, for example `bazel run //mirrord/tests:e2e_setup -- --name mirrord-e2e-ipv6
-- --config mirrord/tests/kind-cluster-ipv6-config.yaml`. Relative paths in them are relative to
the directory that `bazel run` ran in, as for any other program that `bazel run` starts.
"""

import argparse
import os
import shutil
import subprocess
import sys
from pathlib import Path

from python.runfiles import Runfiles

ARGO_ROLLOUTS_MANIFEST = (
    "https://github.com/argoproj/argo-rollouts/releases/download/v1.9.1/install.yaml"
)


def run(command, **kwargs):
    """Runs a command with `subprocess.run`, and returns its result. If the command fails, the
    script exits with the command's exit code, as the command has already printed why it
    failed."""
    result = subprocess.run(command, check=False, **kwargs)
    if result.returncode != 0:
        sys.exit(result.returncode)
    return result


def main():
    parser = argparse.ArgumentParser(
        prog="bazel run //mirrord/tests:e2e_setup --",
        usage="%(prog)s [--runtime kind|minikube] [--name <name>] [--delete] "
        "[-- <arguments for kind or minikube>]",
        description="Creates a cluster for the end-to-end tests, or reuses it if it exists, and "
        "loads the agent into it. `//mirrord/tests:e2e_cluster` takes the same arguments, and "
        "doesn't load the agent.",
    )
    parser.add_argument(
        "--runtime",
        choices=["kind", "minikube"],
        default="kind",
        help="the runtime of the cluster, kind by default",
    )
    parser.add_argument(
        "--name",
        default="mirrord-e2e",
        metavar="<name>",
        help="the name of the cluster, mirrord-e2e by default",
    )
    parser.add_argument(
        "--delete",
        action="store_true",
        help="delete the cluster and its kubeconfig instead",
    )
    parser.add_argument(
        "create_args",
        nargs="*",
        metavar="<arguments for kind or minikube>",
        help="after `--`, the arguments for `kind create cluster` or `minikube start` when the "
        "script creates the cluster",
    )
    # `:e2e_setup` passes the agent's image archive as a runfiles path, and `:e2e_cluster` doesn't
    # pass one.
    parser.add_argument("--agent-image", help=argparse.SUPPRESS)
    arguments = parser.parse_args()

    runtime = arguments.runtime
    name = arguments.name

    for program in ["docker", "kubectl", runtime]:
        if shutil.which(program) is None:
            sys.exit(f"e2e_setup needs `{program}`, which isn't on PATH.")

    kubeconfig = Path(
        os.environ["BUILD_WORKSPACE_DIRECTORY"], "mirrord", "tests", "clusters", runtime, name
    )

    # The cluster's own kubeconfig keeps kind and minikube from editing the default one, which
    # never had this cluster. Both succeed when the cluster doesn't exist, so `--delete` also
    # removes the kubeconfig of a cluster that someone deleted another way.
    if arguments.delete:
        if runtime == "kind":
            run(["kind", "delete", "cluster", "--name", name, "--kubeconfig", kubeconfig])
        else:
            run(
                ["minikube", "delete", "--profile", name],
                env={**os.environ, "KUBECONFIG": str(kubeconfig)},
            )
        kubeconfig.unlink(missing_ok=True)
        return

    # The script finds the archive before it creates the cluster, so that it fails right away
    # when the archive is missing.
    archive = None
    if arguments.agent_image is not None:
        runfiles = Runfiles.Create()
        if runfiles is None:
            sys.exit(
                "e2e_setup can't find its runfiles. Run it with "
                "`bazel run //mirrord/tests:e2e_setup`."
            )
        archive = runfiles.Rlocation(arguments.agent_image)
        if archive is None or not Path(archive).exists():
            sys.exit(
                f"e2e_setup can't find the agent's image in its runfiles: {arguments.agent_image}"
            )

    kubeconfig.parent.mkdir(parents=True, exist_ok=True)
    working_directory = os.environ["BUILD_WORKING_DIRECTORY"]
    kubectl = ["kubectl", "--kubeconfig", kubeconfig]

    # A cluster that exists may still not answer, like a kind cluster whose node container is
    # stopped. The errors of kind and kubectl don't say what to do about it.
    unreachable = (
        f"The {runtime} cluster {name} exists, but it doesn't answer. Delete it with "
        f"`bazel run //mirrord/tests:e2e_setup -- --runtime {runtime} --name {name} --delete`, "
        "and run this again."
    )

    if runtime == "kind":
        clusters = run(["kind", "get", "clusters"], stdout=subprocess.PIPE, text=True).stdout
        if name in clusters.split():
            print(f"Reusing the kind cluster {name}.", flush=True)
            # kind writes the kubeconfig again, in case it's missing, or is the one of an earlier
            # cluster of the same name. It reads the kubeconfig from the node.
            exported = subprocess.run(
                ["kind", "export", "kubeconfig", "--name", name, "--kubeconfig", kubeconfig],
                check=False,
            )
            if exported.returncode != 0:
                sys.exit(unreachable)
        else:
            run(
                [
                    "kind",
                    "create",
                    "cluster",
                    "--name",
                    name,
                    "--kubeconfig",
                    kubeconfig,
                    "--wait",
                    "120s",
                    *arguments.create_args,
                ],
                cwd=working_directory,
            )
    else:
        # minikube reads and writes the cluster's context in the kubeconfig that `KUBECONFIG`
        # names. `minikube status` only succeeds when the cluster runs and that kubeconfig points
        # to it.
        environment = {**os.environ, "KUBECONFIG": str(kubeconfig)}
        status = subprocess.run(
            ["minikube", "status", "--profile", name],
            check=False,
            env=environment,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        if status.returncode == 0:
            print(f"Reusing the minikube cluster {name}.", flush=True)
        else:
            # `minikube start` creates the cluster, and also starts one that is stopped, as after
            # a reboot, or whose last start failed, and it writes the kubeconfig each time. The
            # nodes run containerd, as kind's nodes do, rather than Docker.
            run(
                [
                    "minikube",
                    "start",
                    "--profile",
                    name,
                    "--container-runtime",
                    "containerd",
                    "--wait",
                    "all",
                    *arguments.create_args,
                ],
                cwd=working_directory,
                env=environment,
            )

    ready = subprocess.run(
        [*kubectl, "wait", "--for=condition=Ready", "nodes", "--all", "--timeout=120s"],
        check=False,
    )
    if ready.returncode != 0:
        sys.exit(unreachable)

    # Argo Rollouts, which the rollout tests target. Its manifest creates the controller's
    # Deployment last, so a cluster without the Deployment gets the whole manifest again, including
    # after an install that failed halfway.
    deployment = run(
        [
            *kubectl,
            "get",
            "deployment",
            "argo-rollouts",
            "--namespace",
            "argo-rollouts",
            "--ignore-not-found",
            "--output",
            "name",
        ],
        stdout=subprocess.PIPE,
        text=True,
    ).stdout
    if not deployment.strip():
        namespace = run(
            [
                *kubectl,
                "create",
                "namespace",
                "argo-rollouts",
                "--dry-run=client",
                "--output",
                "yaml",
            ],
            stdout=subprocess.PIPE,
            text=True,
        ).stdout
        run([*kubectl, "apply", "-f", "-"], input=namespace, text=True)
        run([*kubectl, "apply", "--namespace", "argo-rollouts", "-f", ARGO_ROLLOUTS_MANIFEST])

    if archive is None:
        print()
        print(f"The {runtime} cluster {name} runs, without the agent. Load the agent into it with:")
        print()
        print(f"    bazel run //mirrord/tests:e2e_setup -- --runtime {runtime} --name {name}")
        return

    if runtime == "kind":
        run(["kind", "load", "image-archive", archive, "--name", name])
    else:
        run(["minikube", "image", "load", archive, "--profile", name])

    print()
    print(
        f"The {runtime} cluster {name} is ready. Its kubeconfig is {kubeconfig}. "
        "Run the tests with:"
    )
    print()
    print(f"    bazel test //mirrord/tests:e2e --test_env=KUBECONFIG={kubeconfig}")


if __name__ == "__main__":
    main()
