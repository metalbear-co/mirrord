# Storage hand-offs: carrying baggage through an object upload

Pattern: a service issues a signed upload URL → a client uploads → the bucket emits a notification → a queue → a consumer. The notification comes from the cloud provider, not the app, so no request header survives. Carry the baggage **on the object as user metadata**, signed into the URL by the issuer, and restore it in the consumer.

The issuer must be a service that handled the original request, so its context holds the baggage. If uploads aren't issued by the app (third parties, users with standing credentials, batch imports), there's nothing to carry — report the flow as not covered.

## S3 presigned upload → SQS (direct, or S3 → SNS → SQS)

**Issuer** — sign the metadata header into the URL, so the upload fails if the client omits or changes it:

```python
from opentelemetry import propagate
carrier = {}; propagate.inject(carrier)
meta = {"baggage": carrier["baggage"]} if "baggage" in carrier else {}
url = s3.generate_presigned_url("put_object",
    Params={"Bucket": bucket, "Key": key, "Metadata": meta}, ExpiresIn=900)
# return url AND the headers the client must send: {"x-amz-meta-baggage": meta["baggage"]}
```

```go
carrier := propagation.MapCarrier{}
otel.GetTextMapPropagator().Inject(ctx, carrier)
req, _ := presign.PresignPutObject(ctx, &s3.PutObjectInput{
    Bucket: &bucket, Key: &key, Metadata: map[string]string{"baggage": carrier.Get("baggage")}})
// req.SignedHeader includes x-amz-meta-baggage — return it to the client with req.URL
```

Node (`@aws-sdk/s3-request-presigner`): pass `Metadata: { baggage }` on `PutObjectCommand`, and keep `x-amz-meta-baggage` signed. The presigner can hoist unsigned headers into the query string, so check the generated URL actually requires the header.

**Presigned POST** (browser form uploads): add `x-amz-meta-baggage` as a form field *and* an exact-match condition in the policy.

**Uploader (client/frontend)** — send the returned header on the PUT. That's a change to the client; if the client is in another repo or belongs to a third party, list it in the report.

**Consumer** — S3 event notifications don't include user metadata. Fetch it per record:

```python
for rec in json.loads(m["Body"])["Records"]:          # unwrap SNS envelope first if via SNS
    obj = rec["s3"]["object"]
    params = {"Bucket": rec["s3"]["bucket"]["name"], "Key": urllib.parse.unquote_plus(obj["key"])}
    if obj.get("versionId"):
        # On versioned buckets, read the version that produced this event: a later
        # overwrite of the same key carries a different upload's baggage.
        params["VersionId"] = obj["versionId"]
    head = s3.head_object(**params)
    token = context.attach(propagate.extract({"baggage": head["Metadata"].get("baggage", "")}))
    try: handle(rec)
    finally: context.detach(token)
```

The consumer needs `s3:GetObject` (HeadObject is authorised by it), plus `s3:GetObjectVersion` on versioned buckets — flag it if the IAM role lacks them. On unversioned buckets an overwrite before the event is handled still returns the newer upload's metadata; note that race in the report if the app overwrites keys.

**mirrord queue splitting** — on the queue's `queueConfig` `MirrordPropertyList`, set `s3_event: "true"` (plus `sns: "true"` for S3 → SNS → SQS); the operator then fetches the object's user metadata (needs `s3:GetObject`) and exposes it as `S3Metadata`:

```json mirrord=feature
{
  "split_queues": {
    "uploads-queue": {
      "queue_type": "SQS",
      "jq_filter": ".S3Metadata.baggage // \"\" | test(\"mirrord-session=alice\")"
    }
  }
}
```

**S3 → EventBridge → SQS:** the same object-metadata approach works for the consumer, but the message shape differs (EventBridge event, not S3 `Records`). Check whether `s3_event` parsing applies before promising mirrord filtering; otherwise report it as not filterable.

**Alternative when metadata isn't possible:** encode the session in the object key (e.g. `uploads/<mirrord-session>/...`) and filter on `.Body | fromjson | .Records[0].s3.object.key`. Only suggest this if the key layout is the app's to change. It leaks the session into object names.

## GCS signed URL → Pub/Sub

**Issuer** — include the metadata header in the signed headers:

```python
blob = bucket.blob(name)
url = blob.generate_signed_url(version="v4", method="PUT", expiration=900,
    headers={"x-goog-meta-baggage": baggage_value})
# client must send x-goog-meta-baggage with exactly this value
```

```go
url, _ := storage.SignedURL(bucket, name, &storage.SignedURLOptions{
    Scheme: storage.SigningSchemeV4, Method: "PUT", Expires: time.Now().Add(15 * time.Minute),
    Headers: []string{"x-goog-meta-baggage:" + baggageValue}, /* + credentials */ })
```

Node: `file.getSignedUrl({ version: 'v4', action: 'write', extensionHeaders: { 'x-goog-meta-baggage': value } })`.

Resumable uploads: the metadata goes on the session-initiating request — sign/send it there.

**Notification payload format matters.** With `JSON_API_V1` (the default for `gcloud storage buckets notifications create`), the Pub/Sub message `data` is the object resource, including custom `metadata`. With `NONE`, it isn't. Check with `gcloud storage buckets notifications list gs://<bucket>` (read-only). Changing the format is infra — tell the user.

**Consumer:**

```python
def callback(message):
    obj = json.loads(message.data)
    token = context.attach(propagate.extract({"baggage": (obj.get("metadata") or {}).get("baggage", "")}))
    try:
        handle(obj)
        message.ack()          # ack only after success, so failures are redelivered
    except Exception:
        message.nack()
        raise
    finally:
        context.detach(token)
```

Notification **attributes** (`bucketId`, `objectId`, `eventType`, ...) never include custom metadata, so a `message_filter` on `baggage` can't match. Filter on the object's metadata instead:

**mirrord queue splitting** — on the queue's `queueConfig` `MirrordPropertyList`, set `gcs_event: "true"` (operator 3.214.0+); the operator then exposes the notification's object custom metadata as `gcsMetadata`. With a `JSON_API_V1` payload it reads `metadata` off the message data directly; with `NONE` it fetches the object's metadata from Cloud Storage itself (needs `storage.objects.get` on the bucket for the client config's identity — grant it if the consumer's service account lacks it), so this also covers the `NONE` case the in-process consumer above can't:

```json mirrord=feature
{
  "split_queues": {
    "uploads-sub": {
      "queue_type": "GCPPubSub",
      "jq_filter": ".gcsMetadata.baggage // \"\" | test(\"mirrord-session=alice\")"
    }
  }
}
```

**Eventarc / Cloud Run triggers** for GCS events deliver a CloudEvent over HTTP instead of a pull subscription. The object payload still carries `metadata`, but the hop is an HTTP push. Report it separately and check whether the push subscription target is something mirrord can steal from.

## Other object-triggered paths to report

- **Azure Blob → Event Grid → Service Bus/Storage Queue:** Event Grid blob events don't include blob metadata; the consumer must fetch blob properties. No mirrord filter can see it.
- **Copy / multipart / server-side rewrite:** metadata copies by default on S3 `CopyObject` (`MetadataDirective=COPY`) and GCS rewrite, but a `REPLACE` directive drops it. Check any service that post-processes uploads.
- **Lifecycle, replication, and batch jobs:** no request context — not covered.
