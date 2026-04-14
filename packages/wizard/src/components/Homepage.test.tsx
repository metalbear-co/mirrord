import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "../test/test-utils";
import Homepage from "./Homepage";

// Mock the Wizard component
vi.mock("./Wizard", () => ({
  default: ({
    open,
    onClose,
    startWithLearning,
  }: {
    open: boolean;
    onClose: () => void;
    startWithLearning?: boolean;
  }) =>
    open ? (
      <div data-testid="wizard-dialog">
        <span data-testid="wizard-mode">
          {startWithLearning ? "learn" : "config"}
        </span>
        <button data-testid="close-wizard" onClick={onClose}>
          Close
        </button>
      </div>
    ) : null,
}));

describe("Homepage", () => {
  describe("Basic rendering", () => {
    it("renders the Configuration Wizard heading", () => {
      render(<Homepage />);

      expect(screen.getByText("Configuration Wizard")).toBeInTheDocument();
    });

    it("renders the mirrord logo", () => {
      render(<Homepage />);

      expect(screen.getByAltText("mirrord")).toBeInTheDocument();
    });

    it("renders the description text", () => {
      render(<Homepage />);

      expect(screen.getByText(/Generate a/)).toBeInTheDocument();
      expect(screen.getByText("mirrord.json")).toBeInTheDocument();
    });

    it("renders the Get Started button", () => {
      render(<Homepage />);

      expect(screen.getByText("Get Started")).toBeInTheDocument();
    });

    it("renders the learn link with book emoji", () => {
      render(<Homepage />);

      expect(screen.getByText("📖")).toBeInTheDocument();
      expect(
        screen.getByText("New to mirrord? Learn the basics first"),
      ).toBeInTheDocument();
    });
  });

  describe("Wizard interaction", () => {
    it("opens wizard in config mode when Get Started is clicked", () => {
      render(<Homepage />);

      fireEvent.click(screen.getByText("Get Started"));

      expect(screen.getByTestId("wizard-dialog")).toBeInTheDocument();
      expect(screen.getByTestId("wizard-mode")).toHaveTextContent("config");
    });

    it("opens wizard in learn mode when learn link is clicked", () => {
      render(<Homepage />);

      fireEvent.click(
        screen.getByText("New to mirrord? Learn the basics first"),
      );

      expect(screen.getByTestId("wizard-dialog")).toBeInTheDocument();
      expect(screen.getByTestId("wizard-mode")).toHaveTextContent("learn");
    });

    it("does not show wizard by default", () => {
      render(<Homepage />);

      expect(screen.queryByTestId("wizard-dialog")).not.toBeInTheDocument();
    });

    it("closes wizard when close is triggered", () => {
      render(<Homepage />);

      // Open wizard
      fireEvent.click(screen.getByText("Get Started"));
      expect(screen.getByTestId("wizard-dialog")).toBeInTheDocument();

      // Close wizard
      fireEvent.click(screen.getByTestId("close-wizard"));
      expect(screen.queryByTestId("wizard-dialog")).not.toBeInTheDocument();
    });
  });

  describe("Visual elements", () => {
    it("has the or divider between options", () => {
      render(<Homepage />);

      expect(screen.getByText("or")).toBeInTheDocument();
    });

    it("renders the card container", () => {
      render(<Homepage />);

      // The card should have shadow class
      const card = document.querySelector(".shadow-lg");
      expect(card).toBeInTheDocument();
    });
  });
});
