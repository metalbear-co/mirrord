import { Download } from "lucide-react";
import { Button } from "@/components/ui/button";
import { useToast } from "@/hooks/use-toast";

interface DownloadButtonProps {
  json: string;
  filename?: string;
}

export default function DownloadButton({
  json,
  filename = "mirrord-config",
}: DownloadButtonProps) {
  const { toast } = useToast();

  const downloadJson = () => {
    const blob = new Blob([json], { type: "application/json" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `${filename}.json`;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    URL.revokeObjectURL(url);
    toast({
      title: "Downloaded",
      description: "Configuration JSON has been downloaded.",
    });
  };

  return (
    <Button variant="outline" size="sm" onClick={downloadJson}>
      <Download className="h-4 w-4 mr-2" />
      Download JSON
    </Button>
  );
}
