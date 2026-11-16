import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useQueryClient } from "@tanstack/react-query";
import { rulesApi, type Rule } from "@/lib/api";
import { ruleKeys } from "@/lib/query/rules";
import { Button } from "@/components/ui/button";
import { DialogDescription, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { V7Dialog } from "@/components/mcp/formBits";
import MarkdownEditor from "@/components/MarkdownEditor";
import { useDarkMode } from "@/hooks/useDarkMode";
import { toast } from "@/lib/toast";
import { extractErrorMessage } from "@/utils/errorUtils";
import { cn } from "@/lib/utils";

export type Editing = { mode: "create" } | { mode: "edit"; rule: Rule };

// Keep in sync with Rust's validate_rule_name: control characters include C1
// and DEL.
const INVALID_NAME = /[/\\<>:"|?*\u0000-\u001f\u007f-\u009f]/;
const WINDOWS_RESERVED = /^(CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])(\.|$)/i;
const MAX_RULE_CONTENT_BYTES = 256 * 1024;
// File names are capped at 255 bytes; leave room for `.md` while still fitting
// 80 CJK characters.
const MAX_NAME_BYTES = 240;

const byteLength = (value: string) => new TextEncoder().encode(value).length;

export function RuleEditorDialog({
  editing,
  rules,
  onClose,
  onSaved,
}: {
  editing: Editing;
  rules: Rule[];
  onClose: () => void;
  onSaved: (rule: Rule) => void;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const isDarkMode = useDarkMode();
  const source = editing.mode === "edit" ? editing.rule : null;
  const [name, setName] = useState(source?.name ?? "");
  const [content, setContent] = useState(source?.content ?? "");
  const [saving, setSaving] = useState(false);

  const trimmed = name.trim();
  const lower = trimmed.toLowerCase();
  const duplicate = rules.some(
    (rule) =>
      rule.id !== source?.id &&
      (rule.name.toLowerCase() === lower || rule.id.toLowerCase() === lower),
  );
  const lengthError =
    Array.from(trimmed).length > 80 || byteLength(trimmed) > MAX_NAME_BYTES
      ? t("rules.nameTooLong")
      : null;
  const formatError =
    trimmed.startsWith(".") ||
    trimmed.endsWith(".") ||
    INVALID_NAME.test(trimmed) ||
    WINDOWS_RESERVED.test(trimmed)
      ? t("rules.nameInvalid")
      : null;
  const nameError =
    trimmed === ""
      ? null
      : (lengthError ??
        formatError ??
        (duplicate ? t("rules.nameTaken") : null));
  const contentTooLarge = byteLength(content) > MAX_RULE_CONTENT_BYTES;
  const dirty = source
    ? trimmed !== source.name || content !== source.content
    : trimmed !== "" || content !== "";
  const canSave =
    trimmed !== "" && !nameError && !contentTooLarge && dirty && !saving;

  const save = async () => {
    if (!canSave) return;
    setSaving(true);
    try {
      const saved = source
        ? await rulesApi.updateRule(source.id, trimmed, content)
        : await rulesApi.createRule(trimmed, content);
      await queryClient.invalidateQueries({ queryKey: ruleKeys.all });
      toast.success(t(source ? "rules.saved" : "rules.created"));
      onSaved(saved);
    } catch (error) {
      toast.error(t("rules.saveFailed", { error: extractErrorMessage(error) }));
    } finally {
      setSaving(false);
    }
  };

  return (
    <V7Dialog
      open
      width={720}
      className="max-h-[calc(100vh-48px)]"
      onOpenChange={(open) => {
        if (!open && !saving) onClose();
      }}
    >
      <div className="flex shrink-0 flex-col gap-1">
        <DialogTitle className="text-section">
          {t(source ? "rules.editTitle" : "rules.createTitle")}
        </DialogTitle>
        <DialogDescription className="text-caption text-fg-2">
          {t(source ? "rules.editLead" : "rules.createLead")}
        </DialogDescription>
      </div>

      <div className="flex shrink-0 flex-col gap-1.5">
        <label
          className="text-caption font-medium text-fg-2"
          htmlFor="rule-editor-name"
        >
          {t("rules.name")}
        </label>
        <Input
          id="rule-editor-name"
          value={name}
          autoFocus
          aria-invalid={Boolean(nameError)}
          placeholder={t("rules.namePlaceholder")}
          onChange={(event) => setName(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter" && !event.nativeEvent.isComposing) {
              event.preventDefault();
              void save();
            }
          }}
        />
        <p
          className={cn(
            "m-0 text-caption",
            nameError ? "text-danger-text" : "text-fg-3",
          )}
        >
          {nameError ??
            t("rules.nameHint", {
              file: trimmed ? `${trimmed}.md` : t("rules.nameHintPlaceholder"),
            })}
        </p>
      </div>

      <div className="flex min-h-0 flex-col gap-1.5">
        <label className="text-caption font-medium text-fg-2">
          {t("rules.content")}
        </label>
        <div className="overflow-hidden rounded-panel border border-border">
          <MarkdownEditor
            value={content}
            onChange={setContent}
            darkMode={isDarkMode}
            placeholder={t("rules.contentPlaceholder")}
            minHeight="320px"
            maxHeight="45vh"
          />
        </div>
        {contentTooLarge && (
          <p className="m-0 text-caption text-danger-text">
            {t("rules.contentTooLarge")}
          </p>
        )}
      </div>

      <div className="flex shrink-0 justify-end gap-2">
        <Button
          type="button"
          variant="neutral"
          size="regular"
          disabled={saving}
          onClick={onClose}
        >
          {t("rules.cancel")}
        </Button>
        <Button
          type="button"
          variant="solid"
          size="regular"
          disabled={!canSave}
          onClick={() => void save()}
        >
          {saving ? t("rules.saving") : t("rules.save")}
        </Button>
      </div>
    </V7Dialog>
  );
}
