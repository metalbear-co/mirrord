import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "../../test/test-utils";
import ConfigTabs from "./ConfigTabs";
import { ConfigDataContext } from "../UserDataContext";

// Mock child components
vi.mock("./TargetTab", () => ({
  default: ({
    setTargetPorts,
  }: {
    setTargetPorts: (ports: number[]) => void;
  }) => (
    <div data-testid="target-tab">
      <button data-testid="set-ports" onClick={() => setTargetPorts([8080])}>
        Set Ports
      </button>
    </div>
  ),
}));

vi.mock("./NetworkTab", () => ({
  default: () => <div data-testid="network-tab">Network Tab</div>,
}));

// Mock useToast
vi.mock("../../hooks/use-toast", () => ({
  useToast: () => ({
    toast: vi.fn(),
  }),
}));

describe("ConfigTabs", () => {
  const mockSetConfig = vi.fn();
  const mockOnTabChange = vi.fn();
  const mockOnCanAdvanceChange = vi.fn();

  const configWithTarget = {
    target: { path: "deployment/test", namespace: "default" },
    feature: {
      network: {
        incoming: { mode: "mirror" as const },
      },
    },
  };

  const configWithoutTarget = {
    feature: {
      network: {
        incoming: { mode: "mirror" as const },
      },
    },
  };

  const renderWithContext = (
    currentTab: "target" | "network" | "export" = "target",
    config = configWithTarget,
  ) => {
    return render(
      <ConfigDataContext.Provider value={{ config, setConfig: mockSetConfig }}>
        <ConfigTabs
          currentTab={currentTab}
          onTabChange={mockOnTabChange}
          onCanAdvanceChange={mockOnCanAdvanceChange}
        />
      </ConfigDataContext.Provider>,
    );
  };

  beforeEach(() => {
    mockSetConfig.mockClear();
    mockOnTabChange.mockClear();
    mockOnCanAdvanceChange.mockClear();
  });

  describe("Tab Navigation", () => {
    it("renders all three tab buttons", () => {
      renderWithContext();

      expect(screen.getByText("Target")).toBeInTheDocument();
      expect(screen.getByText("Network")).toBeInTheDocument();
      expect(screen.getByText("Export")).toBeInTheDocument();
    });

    it("calls onTabChange when Target tab is clicked", () => {
      renderWithContext("network");

      fireEvent.click(screen.getByText("Target"));

      expect(mockOnTabChange).toHaveBeenCalledWith("target");
    });

    it("calls onTabChange when Network tab is clicked", () => {
      renderWithContext("target");

      fireEvent.click(screen.getByText("Network"));

      expect(mockOnTabChange).toHaveBeenCalledWith("network");
    });

    it("calls onTabChange when Export tab is clicked", () => {
      renderWithContext("target");

      fireEvent.click(screen.getByText("Export"));

      expect(mockOnTabChange).toHaveBeenCalledWith("export");
    });
  });

  describe("Tab Content", () => {
    it("shows TargetTab content when on target tab", () => {
      renderWithContext("target");

      expect(screen.getByTestId("target-tab")).toBeInTheDocument();
    });

    it("shows NetworkTab content when on network tab", () => {
      renderWithContext("network");

      expect(screen.getByTestId("network-tab")).toBeInTheDocument();
    });

    it("shows Export content when on export tab", () => {
      renderWithContext("export");

      expect(screen.getByText("Copy to Clipboard")).toBeInTheDocument();
      expect(screen.getByText("Download File")).toBeInTheDocument();
    });
  });

  describe("Export Tab", () => {
    it("displays the config JSON", () => {
      renderWithContext("export");

      // The config should be displayed in a textarea
      const textarea = screen.getByRole("textbox") as HTMLTextAreaElement;
      expect(textarea).toBeInTheDocument();
      expect(textarea.value).toContain("deployment/test");
    });

    it("has a Copy to Clipboard button", () => {
      renderWithContext("export");

      expect(screen.getByText("Copy to Clipboard")).toBeInTheDocument();
    });

    it("has a Download button", () => {
      renderWithContext("export");

      expect(screen.getByText("Download File")).toBeInTheDocument();
    });
  });

  describe("Can Advance Logic", () => {
    it("calls onCanAdvanceChange with true when target is selected", () => {
      renderWithContext("target", configWithTarget);

      // Should have been called with true since target is selected
      expect(mockOnCanAdvanceChange).toHaveBeenCalledWith(true);
    });

    it("calls onCanAdvanceChange with false when no target is selected", () => {
      renderWithContext("target", configWithoutTarget);

      // Should have been called with false since no target
      expect(mockOnCanAdvanceChange).toHaveBeenCalledWith(false);
    });
  });
});
