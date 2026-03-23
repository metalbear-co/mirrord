import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen } from "../../test/test-utils";
import NetworkTab from "./NetworkTab";
import { ConfigDataContext } from "../UserDataContext";

// Mock child components
vi.mock("./HttpFilter", () => ({
  default: ({ initValue }: { initValue: string }) => (
    <div data-testid="http-filter">{initValue}</div>
  ),
}));

vi.mock("./AddNewFilter", () => ({
  default: ({ type, placeholder }: { type: string; placeholder: string }) => (
    <div data-testid={`add-filter-${type}`}>{placeholder}</div>
  ),
}));

vi.mock("./PortMapping", () => ({
  default: ({ remotePort }: { remotePort: number }) => (
    <div data-testid={`port-mapping-${remotePort}`}>Port {remotePort}</div>
  ),
}));

// Mock useToast
vi.mock("../../hooks/use-toast", () => ({
  useToast: () => ({
    toast: vi.fn(),
    dismiss: vi.fn(),
  }),
}));

describe("NetworkTab", () => {
  const mockSetConfig = vi.fn();
  const mockSetSavedIncoming = vi.fn();
  const mockSetPortConflicts = vi.fn();

  const stealModeConfig = {
    feature: {
      copy_target: { enabled: false, scale_down: false },
      network: {
        incoming: { mode: "steal" as const },
      },
    },
  };

  const mirrorModeConfig = {
    feature: {
      network: {
        incoming: { mode: "mirror" as const },
      },
    },
  };

  const replaceModeConfig = {
    feature: {
      copy_target: { enabled: true, scale_down: true },
      network: {
        incoming: { mode: "steal" as const },
      },
    },
  };

  const renderWithContext = (config = stealModeConfig) => {
    return render(
      <ConfigDataContext.Provider value={{ config, setConfig: mockSetConfig }}>
        <NetworkTab
          savedIncoming={{ mode: "steal" }}
          targetPorts={[8080, 3000]}
          setSavedIncoming={mockSetSavedIncoming}
          setPortConflicts={mockSetPortConflicts}
        />
      </ConfigDataContext.Provider>,
    );
  };

  beforeEach(() => {
    mockSetConfig.mockClear();
    mockSetSavedIncoming.mockClear();
    mockSetPortConflicts.mockClear();
  });

  describe("Incoming Traffic Section", () => {
    it("renders the Incoming Traffic header", () => {
      renderWithContext();

      expect(screen.getByText("Incoming Traffic")).toBeInTheDocument();
    });

    it("renders the incoming traffic toggle", () => {
      renderWithContext();

      // Should have a switch for incoming traffic
      const switches = screen.getAllByRole("switch");
      expect(switches.length).toBeGreaterThan(0);
    });
  });

  describe("Traffic Filtering Section", () => {
    it("shows Traffic Filtering section for steal mode", () => {
      renderWithContext(stealModeConfig);

      expect(screen.getByText("Traffic Filtering")).toBeInTheDocument();
    });

    it("does not show Traffic Filtering for replace mode", () => {
      renderWithContext(replaceModeConfig);

      expect(screen.queryByText("Traffic Filtering")).not.toBeInTheDocument();
    });

    it("shows filter toggle switch", () => {
      renderWithContext(stealModeConfig);

      // There should be multiple "Disabled" labels for different toggles
      const disabledLabels = screen.getAllByText("Disabled");
      expect(disabledLabels.length).toBeGreaterThanOrEqual(1);
    });
  });

  describe("Port Configuration Section", () => {
    it("renders Port Configuration header", () => {
      renderWithContext();

      expect(screen.getByText("Port Configuration")).toBeInTheDocument();
    });

    it("shows port toggle switch", () => {
      renderWithContext();

      // There should be multiple switches (incoming, filters, ports)
      const switches = screen.getAllByRole("switch");
      expect(switches.length).toBeGreaterThanOrEqual(2);
    });

    it("shows different description for replace mode", () => {
      renderWithContext(replaceModeConfig);

      expect(
        screen.getByText(
          /Add port mappings for ports that differ locally and remotely/,
        ),
      ).toBeInTheDocument();
    });

    it("shows different description for non-replace mode", () => {
      renderWithContext(stealModeConfig);

      expect(
        screen.getByText(
          /Add, remove or map ports for traffic mirroring\/stealing/,
        ),
      ).toBeInTheDocument();
    });
  });

  describe("Mode-specific behavior", () => {
    it("handles mirror mode config", () => {
      renderWithContext(mirrorModeConfig);

      // Should render without errors
      expect(screen.getByText("Incoming Traffic")).toBeInTheDocument();
    });

    it("handles steal mode config", () => {
      renderWithContext(stealModeConfig);

      // Should show filtering options for steal mode
      expect(screen.getByText("Traffic Filtering")).toBeInTheDocument();
    });

    it("handles replace mode config", () => {
      renderWithContext(replaceModeConfig);

      // Should not show filtering options for replace mode
      expect(screen.queryByText("Traffic Filtering")).not.toBeInTheDocument();
    });
  });
});
