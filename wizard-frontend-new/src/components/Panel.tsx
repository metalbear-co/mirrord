import { Button } from "@metalbear/ui";

interface PanelProps {
  icon: React.ReactNode;
  title: string;
  description: string;
  buttonText: string;
  onClick: () => void;
  primary?: boolean;
}

const Panel = ({
  icon,
  title,
  description,
  buttonText,
  onClick,
  primary = false,
}: PanelProps) => {
  return (
    <div
      className={`
        card card-hover p-6 flex flex-col h-full
        ${primary ? "border-primary/30" : ""}
      `}
    >
      <div
        className={`
          w-12 h-12 rounded-lg flex items-center justify-center mb-4
          ${primary ? "bg-primary/10 text-primary" : "bg-[var(--muted)] text-[var(--muted-foreground)]"}
        `}
      >
        {icon}
      </div>
      <h3 className="text-lg font-semibold text-[var(--foreground)] mb-2">
        {title}
      </h3>
      <p className="text-sm text-[var(--muted-foreground)] mb-6 flex-grow">
        {description}
      </p>
      <Button
        onClick={onClick}
        variant={primary ? "default" : "outline"}
        className="w-full"
      >
        {buttonText}
      </Button>
    </div>
  );
};

export default Panel;
