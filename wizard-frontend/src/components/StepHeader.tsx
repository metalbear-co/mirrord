import { Badge } from "@/components/ui/badge";
import {
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { ReactNode } from "react";

interface StepHeaderProps {
  title: string;
  description: string;
  titleSpecialFormat?: string;
  badge?: ReactNode;
}

export function StepHeader({
  title,
  description,
  titleSpecialFormat,
  badge,
}: StepHeaderProps) {
  return (
    <DialogHeader className="mb-4">
      <DialogTitle className={titleSpecialFormat || (badge ? "flex items-center gap-2" : "")}>
        {title}
        {badge}
      </DialogTitle>
      <DialogDescription>{description}</DialogDescription>
    </DialogHeader>
  );
}
