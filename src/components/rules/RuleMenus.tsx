import { Check, ChevronDown } from "lucide-react";
import type { AppId } from "@/lib/api";
import { APP_DISPLAY_NAME, AppGlyph } from "@/components/shell/AppGlyph";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { cn } from "@/lib/utils";

/** Single- or multi-select filter dropdown (status, app, etc.). */
export function FilterMenu({
  label,
  active,
  items,
}: {
  label: string;
  active: boolean;
  items: Array<{
    key: string;
    label: string;
    checked: boolean;
    onSelect: () => void;
  }>;
}) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          type="button"
          variant="quiet"
          size="compact"
          className={cn(
            "shrink-0 pe-1.5 ps-2.5",
            active ? "text-fg-1" : "text-fg-2",
          )}
        >
          <span className="max-w-[180px] truncate">{label}</span>
          <ChevronDown className="h-3.5 w-3.5" strokeWidth={2} />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent
        align="start"
        className="min-w-[200px] max-w-[280px]"
      >
        {items.map((item) => (
          <DropdownMenuItem key={item.key} onSelect={item.onSelect}>
            <span className="flex w-4 shrink-0 justify-center">
              {item.checked && (
                <Check className="h-3.5 w-3.5" strokeWidth={2} />
              )}
            </span>
            <span className="min-w-0 flex-1 truncate">{item.label}</span>
          </DropdownMenuItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/** App menu behind "Enable in / Disable in" on the selection toolbar. */
export function AppMenuButton({
  label,
  ariaLabel,
  apps,
  disabled,
  onPick,
}: {
  label: string;
  ariaLabel: string;
  apps: AppId[];
  disabled: boolean;
  onPick: (app: AppId) => void;
}) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          type="button"
          variant="quiet"
          size="compact"
          className="shrink-0 pe-1.5 ps-2"
          disabled={disabled}
        >
          {label}
          <ChevronDown className="h-3.5 w-3.5" strokeWidth={2} />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent
        align="start"
        aria-label={ariaLabel}
        className="min-w-[200px]"
      >
        {apps.map((app) => (
          <DropdownMenuItem key={app} onSelect={() => onPick(app)}>
            <AppGlyph app={app} size={16} badgeClassName="bg-surface" />
            {APP_DISPLAY_NAME[app]}
          </DropdownMenuItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
