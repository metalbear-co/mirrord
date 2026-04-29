import { useState, useEffect } from 'react'
import {
  Button,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from '@metalbear/ui'
import { Check, Copy, ExternalLink, Loader2 } from 'lucide-react'
import type { OperatorWatchStatus } from '../types'

interface ConnectOperatorModalProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  watchStatus: OperatorWatchStatus | null
}

const HELM_REPO_CMD = 'helm repo add metalbear https://metalbear-co.github.io/charts'
const INSTALL_CMD =
  'helm install --set license.key=<YOUR_KEY> mirrord-operator metalbear/mirrord-operator'
const VERIFY_CMD = 'mirrord operator status'

export default function ConnectOperatorModal({
  open,
  onOpenChange,
  watchStatus,
}: ConnectOperatorModalProps) {
  const [step, setStep] = useState(0)

  useEffect(() => {
    if (!open) setStep(0)
  }, [open])

  useEffect(() => {
    if (open && watchStatus?.status === 'watching') {
      const t = setTimeout(() => onOpenChange(false), 1500)
      return () => clearTimeout(t)
    }
  }, [open, watchStatus, onOpenChange])

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-[560px]">
        <DialogHeader>
          <DialogTitle>Connect the mirrord operator</DialogTitle>
          <DialogDescription>Three steps · about 2 minutes</DialogDescription>
        </DialogHeader>

        <Stepper step={step} />

        <div className="min-h-[260px]">
          {step === 0 && <SignupStep onNext={() => setStep(1)} />}
          {step === 1 && (
            <InstallStep onBack={() => setStep(0)} onNext={() => setStep(2)} />
          )}
          {step === 2 && (
            <VerifyStep
              watchStatus={watchStatus}
              onBack={() => setStep(1)}
              onClose={() => onOpenChange(false)}
            />
          )}
        </div>
      </DialogContent>
    </Dialog>
  )
}

function Stepper({ step }: { step: number }) {
  const labels = ['Sign up', 'Install', 'Verify']
  return (
    <div className="flex items-center gap-2 pb-3 border-b border-border mb-4">
      {labels.map((label, i) => (
        <div key={label} className="flex items-center gap-2 flex-1">
          <div
            className={`flex items-center gap-1.5 text-xs font-semibold ${
              step >= i ? 'text-primary' : 'text-muted-foreground'
            }`}
          >
            <span
              className={`inline-flex items-center justify-center w-5 h-5 rounded-full text-[11px] font-bold ${
                step > i
                  ? 'bg-primary text-primary-foreground'
                  : step === i
                    ? 'bg-primary/15 text-primary'
                    : 'bg-muted text-muted-foreground'
              }`}
            >
              {step > i ? '✓' : i + 1}
            </span>
            {label}
          </div>
          {i < labels.length - 1 && (
            <div
              className={`flex-1 h-px ${step > i ? 'bg-primary' : 'bg-border'}`}
            />
          )}
        </div>
      ))}
    </div>
  )
}

function SignupStep({ onNext }: { onNext: () => void }) {
  return (
    <div className="flex flex-col gap-4">
      <div>
        <h3 className="text-sm font-bold mb-1">1. Sign up at app.metalbear.com</h3>
        <p className="text-xs text-muted-foreground leading-relaxed">
          Create an account to get a license key. Your key activates the
          operator on any cluster you run mirrord against. 14-day trial, no
          card required.
        </p>
      </div>
      <ul className="flex flex-col gap-2">
        <Bullet>Concurrent debugging on the same target via HTTP filters</Bullet>
        <Bullet>Queue splitting (SQS, Kafka, RabbitMQ) and DB branching</Bullet>
        <Bullet>MirrordPolicy CRDs for platform guardrails</Bullet>
      </ul>
      <div className="flex items-center gap-3">
        <Button asChild>
          <a
            href="https://app.metalbear.com"
            target="_blank"
            rel="noreferrer"
            onClick={onNext}
            className="inline-flex items-center gap-1.5"
          >
            Open app.metalbear.com <ExternalLink className="h-3.5 w-3.5" />
          </a>
        </Button>
        <button
          type="button"
          onClick={onNext}
          className="text-xs text-muted-foreground hover:text-foreground"
        >
          I already have a key →
        </button>
      </div>
    </div>
  )
}

function InstallStep({ onBack, onNext }: { onBack: () => void; onNext: () => void }) {
  const rows = [
    { label: 'Add the Helm repository', cmd: HELM_REPO_CMD },
    { label: 'Install the operator', cmd: INSTALL_CMD },
    { label: 'Verify it picked up your license', cmd: VERIFY_CMD },
  ]
  return (
    <div className="flex flex-col gap-4">
      <div>
        <h3 className="text-sm font-bold mb-1">2. Install on your cluster</h3>
        <p className="text-xs text-muted-foreground leading-relaxed">
          Run these against your current kube context. Replace{' '}
          <code className="font-mono text-[11px] px-1 rounded bg-muted">
            &lt;YOUR_KEY&gt;
          </code>{' '}
          with the license from step 1.
        </p>
      </div>
      <div className="flex flex-col gap-2">
        {rows.map((row, i) => (
          <CommandRow key={i} label={row.label} cmd={row.cmd} />
        ))}
      </div>
      <p className="text-xs text-muted-foreground">
        Prefer Terraform or Argo CD?{' '}
        <a
          href="https://metalbear.com/mirrord/docs/managing-mirrord/operator"
          target="_blank"
          rel="noreferrer"
          className="text-primary hover:underline"
        >
          See the operator install docs
        </a>
        .
      </p>
      <div className="flex justify-between items-center">
        <button
          type="button"
          onClick={onBack}
          className="text-xs text-muted-foreground hover:text-foreground"
        >
          ← Back
        </button>
        <Button onClick={onNext}>I&apos;ve installed it →</Button>
      </div>
    </div>
  )
}

function VerifyStep({
  watchStatus,
  onBack,
  onClose,
}: {
  watchStatus: OperatorWatchStatus | null
  onBack: () => void
  onClose: () => void
}) {
  const isWatching = watchStatus?.status === 'watching'
  return (
    <div className="flex flex-col gap-4">
      <div>
        <h3 className="text-sm font-bold mb-1">3. Verify connection</h3>
        <p className="text-xs text-muted-foreground leading-relaxed">
          mirrord ui will detect the operator on your kube context. This
          usually takes a few seconds after install.
        </p>
      </div>
      <div className="flex items-center gap-3 px-4 py-3 rounded-lg bg-card border border-border">
        {isWatching ? (
          <Check className="h-5 w-5 text-primary" />
        ) : (
          <Loader2 className="h-5 w-5 text-primary animate-spin" />
        )}
        <div>
          <div className="text-xs font-semibold">
            {isWatching ? 'Operator detected' : 'Watching your cluster…'}
          </div>
          <div className="text-[11px] text-muted-foreground">
            {isWatching
              ? 'Closing this dialog now.'
              : 'Polling every 5s for the operator CRD'}
          </div>
        </div>
      </div>
      <p className="text-xs text-muted-foreground">
        Stuck?{' '}
        <a
          href="https://metalbear.com/mirrord/docs/troubleshooting/"
          target="_blank"
          rel="noreferrer"
          className="text-primary hover:underline"
        >
          Troubleshooting guide
        </a>
      </p>
      <div className="flex justify-between items-center">
        <button
          type="button"
          onClick={onBack}
          className="text-xs text-muted-foreground hover:text-foreground"
        >
          ← Back
        </button>
        <Button onClick={onClose}>Done</Button>
      </div>
    </div>
  )
}

function CommandRow({ label, cmd }: { label: string; cmd: string }) {
  const [copied, setCopied] = useState(false)
  return (
    <div>
      <div className="text-[11px] font-semibold text-muted-foreground mb-1">
        {label}
      </div>
      <div className="relative">
        <pre className="m-0 p-3 pr-16 bg-zinc-900 text-zinc-100 rounded-lg font-mono text-xs leading-relaxed overflow-x-auto whitespace-nowrap">
          {cmd}
        </pre>
        <button
          type="button"
          onClick={() => {
            navigator.clipboard?.writeText(cmd)
            setCopied(true)
            setTimeout(() => setCopied(false), 1500)
          }}
          className="absolute top-1.5 right-1.5 px-2 py-1 rounded text-[11px] font-semibold border border-zinc-700 bg-zinc-800 text-zinc-100 hover:bg-zinc-700"
        >
          {copied ? 'Copied' : 'Copy'}
        </button>
      </div>
    </div>
  )
}

function Bullet({ children }: { children: React.ReactNode }) {
  return (
    <li className="flex gap-2 items-start text-xs">
      <span className="inline-flex items-center justify-center w-4 h-4 rounded-full bg-primary/15 text-primary text-[10px] font-bold flex-shrink-0 mt-0.5">
        ✓
      </span>
      <span>{children}</span>
    </li>
  )
}
