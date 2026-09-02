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

from temporalio import activity, workflow
from temporalio.client import Client
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


async def main() -> None:
    address = os.environ.get("TEMPORAL_ADDRESS", "localhost:7233")
    namespace = os.environ.get("TEMPORAL_NAMESPACE", "default")
    task_queue = os.environ.get("TEMPORAL_TASK_QUEUE", "order-checkout")

    print("temporal-worker-py starting", file=sys.stderr, flush=True)
    print(f"  TEMPORAL_ADDRESS={address}", file=sys.stderr, flush=True)
    print(f"  TEMPORAL_NAMESPACE={namespace}", file=sys.stderr, flush=True)
    print(f"  TEMPORAL_TASK_QUEUE={task_queue}", file=sys.stderr, flush=True)

    client = await Client.connect(address, namespace=namespace)
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
