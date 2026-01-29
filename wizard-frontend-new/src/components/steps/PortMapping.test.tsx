import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent } from '../../test/test-utils';
import PortMappingEntry from './PortMapping';
import { ConfigDataContext } from '../UserDataContext';

// Mock useToast
vi.mock('../../hooks/use-toast', () => ({
  useToast: () => ({
    toast: vi.fn(),
    dismiss: vi.fn(),
  }),
}));

describe('PortMappingEntry', () => {
  const mockSetConfig = vi.fn();
  const mockSetPortConflicts = vi.fn();
  const mockConfig = {
    feature: {
      copy_target: { enabled: false, scale_down: false },
      network: {
        incoming: {
          mode: 'steal' as const,
          ports: [8080, 3000],
          port_mapping: [[9000, 8080] as [number, number]],
        },
      },
    },
  };

  const renderWithContext = (
    remotePort = 8080,
    detectedPort = true,
    config = mockConfig
  ) => {
    return render(
      <ConfigDataContext.Provider value={{ config, setConfig: mockSetConfig }}>
        <PortMappingEntry
          remotePort={remotePort}
          detectedPort={detectedPort}
          setPortConflicts={mockSetPortConflicts}
        />
      </ConfigDataContext.Provider>
    );
  };

  beforeEach(() => {
    mockSetConfig.mockClear();
    mockSetPortConflicts.mockClear();
  });

  it('displays the remote port', () => {
    renderWithContext();

    expect(screen.getByText('8080')).toBeInTheDocument();
  });

  it('displays the mapped local port in input', () => {
    renderWithContext();

    // 8080 is mapped to 9000 in the mock config
    const input = screen.getByDisplayValue('9000');
    expect(input).toBeInTheDocument();
  });

  it('renders the delete button for non-replace mode', () => {
    renderWithContext();

    const deleteButton = screen.getByRole('button');
    expect(deleteButton).toBeInTheDocument();
  });

  it('calls setConfig when local port is changed', () => {
    renderWithContext();

    const input = screen.getByDisplayValue('9000');
    fireEvent.change(input, { target: { value: '9001' } });

    expect(mockSetConfig).toHaveBeenCalled();
  });

  it('calls setConfig when delete button is clicked', () => {
    renderWithContext();

    const deleteButton = screen.getByRole('button');
    fireEvent.click(deleteButton);

    expect(mockSetConfig).toHaveBeenCalled();
  });

  it('displays same port when no mapping exists', () => {
    const configWithoutMapping = {
      feature: {
        copy_target: { enabled: false, scale_down: false },
        network: {
          incoming: {
            mode: 'steal' as const,
            ports: [3000],
          },
        },
      },
    };

    renderWithContext(3000, true, configWithoutMapping);

    // 3000 should display as 3000 since no mapping exists
    const input = screen.getByDisplayValue('3000');
    expect(input).toBeInTheDocument();
  });

  it('hides delete button in replace mode for detected ports', () => {
    const replaceConfig = {
      feature: {
        copy_target: { enabled: true, scale_down: true },
        network: {
          incoming: {
            mode: 'steal' as const,
            ports: [8080],
          },
        },
      },
    };

    renderWithContext(8080, true, replaceConfig);

    // In replace mode with detected port, delete button should not be visible
    expect(screen.queryByRole('button')).not.toBeInTheDocument();
  });
});
