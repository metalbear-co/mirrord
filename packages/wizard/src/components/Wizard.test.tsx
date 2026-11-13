import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { render, screen, fireEvent, waitFor } from '../test/test-utils'
import { installFakeServer, json, renderRequests } from '../test/fake-server'
import Wizard from './Wizard'

vi.mock('../hooks/use-toast', () => ({
  useToast: () => ({ toast: vi.fn() }),
}))

const next = () => fireEvent.click(screen.getByRole('button', { name: /Next/ }))
const back = () => fireEvent.click(screen.getByRole('button', { name: /Back/ }))
const nextButton = () => screen.getByRole('button', { name: /Next/ })

/** Picks the mode with `label` and goes on to the target step. */
const modeCard = (label: string) =>
  screen.findByRole('button', { name: new RegExp(`^${label}`) })

const pickMode = async (label: string) => {
  fireEvent.click(await modeCard(label))
  fireEvent.click(screen.getByRole('button', { name: /Continue/ }))
}

const pickOption = async (text: RegExp | string) =>
  fireEvent.click(
    await screen.findByRole('button', { name: text }, { timeout: 2000 }),
  )

const typeInto = (label: RegExp, value: string) =>
  fireEvent.change(screen.getByLabelText(label), { target: { value } })

describe('Wizard', () => {
  const onClose = vi.fn()

  beforeEach(() => {
    installFakeServer()
    onClose.mockClear()
  })

  afterEach(() => {
    vi.unstubAllGlobals()
  })

  it('starts on the mode question, with the modes of `mirrord up init`', async () => {
    render(<Wizard open={true} onClose={onClose} />)

    expect(screen.getByText('Choose Mode')).toBeInTheDocument()
    expect(
      screen.getByText('How do you want to interact with remote traffic?'),
    ).toBeInTheDocument()
    expect(await modeCard('Split')).toHaveTextContent('Recommended')
    expect(await modeCard('Mirror')).toBeInTheDocument()
    expect(await modeCard('Replace')).toBeInTheDocument()
  })

  // jsdom does no layout, so check the structure that keeps Back and Next in view.
  it('keeps the footer out of the scrolling step', async () => {
    render(<Wizard open={true} onClose={onClose} />)

    await pickMode('Split')
    const step = screen.getByTestId('wizard-step')
    expect(step).toHaveClass('overflow-y-auto', 'min-h-0', 'flex-1')
    expect(step).not.toContainElement(nextButton())
  })

  it('collects a service and sends it to be rendered', async () => {
    render(<Wizard open={true} onClose={onClose} />)

    await pickMode('Split')
    expect(await screen.findByText('minikube')).toBeInTheDocument()
    expect(nextButton()).toBeDisabled()
    await pickOption(/server-echo deployment/)
    expect(
      screen.getByText('server-echo', { selector: 'p' }),
    ).toBeInTheDocument()
    next()

    fireEvent.click(
      screen.getByLabelText('Customize the header filter and ports?'),
    )
    typeInto(/HTTP header filter/, 'x-user: me')
    fireEvent.click(await screen.findByLabelText('intercept port 9090'))
    typeInto(/Other ports to ignore/, '15090')
    next()

    typeInto(/Local command/, 'node server.js')
    fireEvent.click(screen.getByRole('button', { name: /Add Service/ }))

    expect(await screen.findByText('Export Configuration')).toBeInTheDocument()
    await waitFor(() => expect(renderRequests).toHaveLength(1))
    expect(renderRequests[0]).toEqual([
      {
        name: 'server-echo',
        mode: 'split',
        target: { path: 'deployment/server-echo', namespace: 'default' },
        header_filter: 'x-user: me',
        ignore_ports: [9090, 15090],
        command: 'node server.js',
        directory: null,
      },
    ])
    expect(screen.getByText('mirrord up -f <file>')).toBeInTheDocument()
    expect(
      screen.getByRole('link', { name: 'See the common settings' }),
    ).toHaveAttribute(
      'href',
      'https://metalbear.com/mirrord/docs/using-mirrord/multiple-concurrent-sessions/#common',
    )
    expect(screen.queryByText(/VSCode|JetBrains/)).not.toBeInTheDocument()
  })

  it('picks the target again after the mode changes', async () => {
    render(<Wizard open={true} onClose={onClose} />)

    await pickMode('Split')
    await pickOption(/server-echo-abc\/container\/echo pod/)
    back()
    await pickMode('Replace')

    expect(nextButton()).toBeDisabled()
    expect(screen.queryByText('Service name:')).not.toBeInTheDocument()
  })

  it('shows why the cluster cannot be read', async () => {
    installFakeServer({
      '/api/v1/up/workloads': () =>
        json({ error: 'kube api request failed: Unauthorized' }, 502),
    })
    render(<Wizard open={true} onClose={onClose} />)

    await pickMode('Split')
    expect(
      await screen.findByText(
        'Could not list the workloads: kube api request failed: Unauthorized',
      ),
    ).toBeInTheDocument()
  })

  it('offers no targetless option and no header filter in replace mode', async () => {
    render(<Wizard open={true} onClose={onClose} />)

    await pickMode('Replace')
    await pickOption(/server-echo deployment/)
    expect(
      screen.queryByRole('button', { name: /Run without a target/ }),
    ).not.toBeInTheDocument()
    next()

    fireEvent.click(screen.getByLabelText('Customize ports?'))
    expect(
      screen.queryByLabelText(/HTTP header filter/),
    ).not.toBeInTheDocument()
  })

  it('does not go on with a port that does not parse', async () => {
    render(<Wizard open={true} onClose={onClose} />)

    await pickMode('Split')
    await pickOption(/server-echo deployment/)
    next()
    fireEvent.click(
      screen.getByLabelText('Customize the header filter and ports?'),
    )
    await screen.findByLabelText('intercept port 80')
    typeInto(/Other ports to ignore/, 'http')

    expect(
      screen.getByText('`http` is not a valid u16 port'),
    ).toBeInTheDocument()
    expect(nextButton()).toBeDisabled()
  })

  it('skips the traffic questions for a targetless service', async () => {
    render(<Wizard open={true} onClose={onClose} />)

    await pickMode('Split')
    await pickOption(/Run without a target/)
    expect(nextButton()).toBeDisabled()
    typeInto(/Service name/, 'worker')
    next()

    expect(
      screen.getByText('Local Command', { selector: 'h2' }),
    ).toBeInTheDocument()
    back()
    expect(screen.getByText('Choose Target')).toBeInTheDocument()
  })

  it('adds another service, named after its workload', async () => {
    render(<Wizard open={true} onClose={onClose} />)

    for (let i = 0; i < 2; i++) {
      await pickMode('Split')
      await pickOption(/server-echo deployment/)
      next()
      next()
      typeInto(/Local command/, 'node server.js')
      fireEvent.click(screen.getByRole('button', { name: /Add Service/ }))
      await screen.findByText('Export Configuration')
      if (i === 0) {
        fireEvent.click(
          screen.getByRole('button', { name: /Add another service/ }),
        )
      }
    }

    await waitFor(() =>
      expect(renderRequests.at(-1)?.map((s) => s.name)).toEqual([
        'server-echo',
        'server-echo-2',
      ]),
    )
  })

  it('goes back from a new service to the services so far', async () => {
    render(<Wizard open={true} onClose={onClose} />)

    await pickMode('Split')
    await pickOption(/Run without a target/)
    typeInto(/Service name/, 'worker')
    next()
    typeInto(/Local command/, 'python worker.py')
    fireEvent.click(screen.getByRole('button', { name: /Add Service/ }))
    fireEvent.click(
      await screen.findByRole('button', { name: /Add another service/ }),
    )

    expect(screen.getByText('Choose Mode')).toBeInTheDocument()
    back()
    expect(screen.getByText('Export Configuration')).toBeInTheDocument()
    expect(screen.getByText('worker')).toBeInTheDocument()
  })

  describe('learning', () => {
    it('starts with the learning pages and can skip them', () => {
      render(<Wizard open={true} onClose={onClose} startWithLearning={true} />)

      expect(screen.getByText('Learn mirrord')).toBeInTheDocument()
      fireEvent.click(screen.getByRole('button', { name: 'Skip' }))
      expect(screen.getByText('Choose Mode')).toBeInTheDocument()
    })

    it('goes back to the learning pages from the mode question', () => {
      render(<Wizard open={true} onClose={onClose} startWithLearning={true} />)

      for (let i = 0; i < 6; i++) {
        fireEvent.click(
          screen.getByRole('button', { name: /^(Next|Start Configuration)/ }),
        )
      }
      expect(screen.getByText('Choose Mode')).toBeInTheDocument()
      back()
      expect(screen.getByText('Learn mirrord')).toBeInTheDocument()
    })
  })
})
