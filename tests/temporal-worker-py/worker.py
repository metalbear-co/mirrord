# Python twin of tests/temporal-worker, same workflow/activity names, env vars,
# and output contract ("Started Worker" when polling, "1:<workflow_id>" when a
# workflow completes).
#
# It exists because the two Temporal SDK families resolve a workflow's task
# queue differently. The Go SDK reads it from the WorkflowExecutionStarted
# history event - the origin queue as the server recorded it - so activities
# scheduled with default options stay valid even when mirrord patches the
# worker onto a virtual queue. sdk-core (Python/TypeScript/.NET/Ruby) uses the
# worker's configured queue - the patched virtual name - so the operator must
# rewrite it back to the origin in the workflow task completion, or the server
# rejects every completion with BadScheduleActivityAttributes. Only a worker
# from this family exercises that rewrite.

import asyncio
import os
import sys
from datetime import timedelta

import temporalio.service
from temporalio import activity, workflow
from temporalio.client import Client
from temporalio.exceptions import WorkflowAlreadyStartedError
from temporalio.service import RPCError
from temporalio.worker import Worker


@activity.defn(name="ProcessOrder")
async def process_order(order_id: str) -> str:
    info = activity.info()
    print(
        f"activity workflow_id={info.workflow_id} "
        f"activity_type={info.activity_type} order_id={order_id}",
        flush=True,
    )
    return f"processed:{order_id}"


@workflow.defn(name="CheckoutWorkflow")
class CheckoutWorkflow:
    @workflow.run
    async def run(self, order_id: str) -> str:
        # No explicit task_queue: sdk-core fills in the worker's configured
        # queue, the shape the completion rewrite exists for.
        result = await workflow.execute_activity(
            "ProcessOrder",
            order_id,
            start_to_close_timeout=timedelta(minutes=1),
        )
        # Lines starting with "1:" are asserted by operator E2E tests (same
        # convention as the Go worker and Pub/Sub consumers).
        print(f"1:{workflow.info().workflow_id}", flush=True)
        return result


async def probe_already_started(client: Client, task_queue: str, workflow_id: str) -> None:
    """Starts one workflow twice with the same id through the worker's own client,
    which under mirrord means through the operator's proxy. The duplicate must raise
    WorkflowAlreadyStartedError. The server sends ALREADY_EXISTS plus a
    WorkflowExecutionAlreadyStartedFailure in grpc-status-details-bin, and the SDK
    only raises the typed error when those details arrive; a proxy that drops them
    leaves a bare RPCError, and code that handles the expected duplicate never runs.
    Lines starting with "1:" are asserted by operator E2E tests."""
    await client.start_workflow(
        "CheckoutWorkflow", workflow_id, id=workflow_id, task_queue=task_queue
    )
    try:
        await client.start_workflow(
            "CheckoutWorkflow", workflow_id, id=workflow_id, task_queue=task_queue
        )
    except WorkflowAlreadyStartedError:
        print("1:already-started:typed", flush=True)
    except RPCError as error:
        print(f"1:already-started:untyped:{error}", flush=True)
    else:
        print("1:already-started:no-error", flush=True)


async def main() -> None:
    address = os.environ.get("TEMPORAL_ADDRESS", "localhost:7233")
    namespace = os.environ.get("TEMPORAL_NAMESPACE", "default")
    task_queue = os.environ.get("TEMPORAL_TASK_QUEUE", "order-checkout")

    print("temporal-worker-py starting", file=sys.stderr, flush=True)
    print(f"  TEMPORAL_ADDRESS={address}", file=sys.stderr, flush=True)
    print(f"  TEMPORAL_NAMESPACE={namespace}", file=sys.stderr, flush=True)
    print(f"  TEMPORAL_TASK_QUEUE={task_queue}", file=sys.stderr, flush=True)

    connect_kwargs = {}
    compression = os.environ.get("TEMPORAL_GRPC_COMPRESSION")
    if compression:
        # Releases with GrpcCompression gzip every request by default; older ones
        # always send uncompressed and have no knob, so asking for one is an error
        # rather than a silent no-op.
        modes = getattr(temporalio.service, "GrpcCompression", None)
        if modes is None:
            sys.exit(
                f"TEMPORAL_GRPC_COMPRESSION={compression} needs a temporalio "
                "release that has temporalio.service.GrpcCompression"
            )
        print(f"  TEMPORAL_GRPC_COMPRESSION={compression}", file=sys.stderr, flush=True)
        connect_kwargs["grpc_compression"] = getattr(modes, compression.upper())

    client = await Client.connect(address, namespace=namespace, **connect_kwargs)

    probe_id = os.environ.get("TEMPORAL_PROBE_ALREADY_STARTED")
    if probe_id:
        # Under mirrord TEMPORAL_TASK_QUEUE is the session's virtual queue, which only
        # the operator serves. A workflow started on it would sit on the server forever,
        # so the probe is told the original queue separately.
        probe_queue = os.environ.get("TEMPORAL_PROBE_TASK_QUEUE", task_queue)
        print(f"  probe: starting {probe_id} twice on {probe_queue}", file=sys.stderr, flush=True)
        await probe_already_started(client, probe_queue, probe_id)

    worker = Worker(
        client,
        task_queue=task_queue,
        workflows=[CheckoutWorkflow],
        activities=[process_order],
    )
    print("Started Worker", flush=True)
    await worker.run()


if __name__ == "__main__":
    asyncio.run(main())
