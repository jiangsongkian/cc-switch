import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useQueryClient } from "@tanstack/react-query";
import {
  Loader2,
  MoreHorizontal,
  Plus,
  RefreshCw,
  ScrollText,
} from "lucide-react";
import { toast } from "@/lib/toast";
import {
  rulesApi,
  type AppId,
  type DiscoverableRule,
  type Rule,
  type RuleApps,
} from "@/lib/api";
import {
  ruleKeys,
  useDiscoverableRules,
  useRuleAppSupport,
  useRules,
} from "@/lib/query/rules";
import { AppPageHeader } from "@/components/shell/AppPageHeader";
import { APP_DISPLAY_NAME, AppGlyph } from "@/components/shell/AppGlyph";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { HelpTip } from "@/components/ui/help-tip";
import { PageTabs } from "@/components/ui/page-tabs";
import {
  MatrixCell,
  MatrixColumnHeader,
  MatrixColumnHighlight,
  MatrixSearch,
  matrixMinWidth,
  NeutralBadge,
  resolveBulkScope,
  showUndoToast,
} from "@/components/mcp/AppMatrix";
import { CHECKBOX_CLASS, V7ConfirmDialog } from "@/components/mcp/formBits";
import { useVisibleAppIds } from "@/components/mcp/useVisibleAppIds";
import { RuleEditorDialog, type Editing } from "./RuleEditorDialog";
import { AppMenuButton, FilterMenu } from "./RuleMenus";
import { extractErrorMessage } from "@/utils/errorUtils";
import { cn } from "@/lib/utils";

type RulesView = "installed" | "discover";
type StatusFilter = "all" | "enabled" | "disabled" | `app:${AppId}`;
type CellFailure = { desired: boolean; error: string };

const cellKey = (id: string, app: AppId) => `${id}\u0000${app}`;
const appOn = (apps: RuleApps, app: AppId) =>
  Boolean((apps as Partial<Record<AppId, boolean>>)[app]);

export function RulesPage() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const rulesQuery = useRules();
  const supportQuery = useRuleAppSupport();
  const rules = rulesQuery.data ?? [];
  const support = supportQuery.data ?? [];
  const supportAppIds = useMemo(
    () => support.map((item) => item.app),
    [support],
  );
  const visibleAppIds = useVisibleAppIds(supportAppIds);

  const [view, setView] = useState<RulesView>("installed");
  const [searchQuery, setSearchQuery] = useState("");
  const [discoverQuery, setDiscoverQuery] = useState("");
  const [statusFilter, setStatusFilter] = useState<StatusFilter>("all");
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [editing, setEditing] = useState<Editing | null>(null);
  const [confirmDelete, setConfirmDelete] = useState<Rule | null>(null);
  const [fails, setFails] = useState<Record<string, CellFailure>>({});
  const [pendingCells, setPendingCells] = useState<Set<string>>(new Set());
  const [importing, setImporting] = useState<Set<string>>(new Set());
  const [syncing, setSyncing] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const [highlightId, setHighlightId] = useState<string | null>(null);
  const pendingCellsRef = useRef(new Set<string>());

  const discoverQueryResult = useDiscoverableRules(view === "discover");
  const discovered = discoverQueryResult.data ?? [];

  const enabledAppCount = (rule: Rule) =>
    supportAppIds.filter((app) => appOn(rule.apps, app)).length;

  const normalizedQuery = searchQuery.trim().toLocaleLowerCase();
  const filteredRules = useMemo(() => {
    const isEnabled = (rule: Rule) =>
      supportAppIds.some((app) => appOn(rule.apps, app));
    return rules.filter((rule) => {
      if (statusFilter === "enabled" && !isEnabled(rule)) return false;
      if (statusFilter === "disabled" && isEnabled(rule)) return false;
      if (
        statusFilter.startsWith("app:") &&
        !appOn(rule.apps, statusFilter.slice(4) as AppId)
      ) {
        return false;
      }
      if (!normalizedQuery) return true;
      return [rule.name, rule.content].some((value) =>
        value.toLocaleLowerCase().includes(normalizedQuery),
      );
    });
  }, [rules, statusFilter, normalizedQuery, supportAppIds]);

  const filtersActive = statusFilter !== "all";
  const scope = resolveBulkScope(
    rules,
    filteredRules,
    normalizedQuery ? "search" : filtersActive ? "filter" : null,
  );

  // Discover lists only files that are not managed yet; added ones disappear
  // from the list.
  const unaddedDiscovered = useMemo(
    () => discovered.filter((file) => !file.managed),
    [discovered],
  );
  const allDiscoveredAdded =
    discovered.length > 0 && unaddedDiscovered.length === 0;

  const normalizedDiscoverQuery = discoverQuery.trim().toLocaleLowerCase();
  const filteredDiscovered = useMemo(
    () =>
      unaddedDiscovered.filter((file) => {
        if (!normalizedDiscoverQuery) return true;
        return [
          file.name,
          file.preview,
          file.displayPath,
          ...file.apps.map((app) => APP_DISPLAY_NAME[app]),
        ].some((value) =>
          value.toLocaleLowerCase().includes(normalizedDiscoverQuery),
        );
      }),
    [unaddedDiscovered, normalizedDiscoverQuery],
  );

  // Drop selections for rules that are no longer in the list.
  useEffect(() => {
    if (selected.size === 0) return;
    const ids = new Set(rules.map((rule) => rule.id));
    const next = new Set([...selected].filter((id) => ids.has(id)));
    if (next.size !== selected.size) setSelected(next);
  }, [rules, selected]);

  // Scroll to the row after returning from Discover or the editor.
  useEffect(() => {
    if (!highlightId || view !== "installed") return;
    const row = document.getElementById(`rule-row-${highlightId}`);
    row?.scrollIntoView?.({ block: "center" });
    row?.focus?.({ preventScroll: true });
  }, [highlightId, view, rules]);

  const beginCell = (key: string) => {
    if (pendingCellsRef.current.has(key)) return false;
    pendingCellsRef.current.add(key);
    setPendingCells((prev) => new Set(prev).add(key));
    return true;
  };

  const endCell = (key: string) => {
    pendingCellsRef.current.delete(key);
    setPendingCells((prev) => {
      const next = new Set(prev);
      next.delete(key);
      return next;
    });
  };

  const toggleApp = async (
    rule: Rule,
    app: AppId,
    enabled: boolean,
  ): Promise<boolean> => {
    const key = cellKey(rule.id, app);
    if (!beginCell(key)) return false;
    setFails((prev) => {
      const next = { ...prev };
      delete next[key];
      return next;
    });
    try {
      const updated = await rulesApi.toggleApp(rule.id, app, enabled);
      queryClient.setQueryData<Rule[]>(ruleKeys.list(), (old) =>
        old?.map((item) => (item.id === updated.id ? updated : item)),
      );
      return true;
    } catch (error) {
      setFails((prev) => ({
        ...prev,
        [key]: { desired: enabled, error: extractErrorMessage(error) },
      }));
      return false;
    } finally {
      endCell(key);
    }
  };

  const bulkToggle = async (
    targets: readonly Rule[],
    app: AppId,
    enabled: boolean,
  ) => {
    const changed = targets.filter((rule) => appOn(rule.apps, app) !== enabled);
    if (changed.length === 0) return;
    const done: Rule[] = [];
    for (const rule of changed) {
      if (await toggleApp(rule, app, enabled)) done.push(rule);
    }
    const failed = changed.length - done.length;
    let text = t(
      enabled ? "appMatrix.toast.enabled" : "appMatrix.toast.disabled",
      {
        count: done.length,
        noun: t("rules.noun"),
        app: APP_DISPLAY_NAME[app],
      },
    );
    if (failed) text += t("appMatrix.toast.partialFail", { count: failed });
    showUndoToast(
      text,
      t("appMatrix.undo"),
      done.length
        ? () => {
            void bulkToggle(done, app, !enabled);
          }
        : undefined,
      failed ? (done.length ? "warning" : "error") : "success",
    );
  };

  const refreshDiscover = () => {
    void discoverQueryResult.refetch();
  };

  const applyImport = async (names: string[]) => {
    try {
      const result = await rulesApi.importRules(names);
      await queryClient.invalidateQueries({ queryKey: ruleKeys.all });
      if (result.rules.length > 0) {
        toast.success(
          t("rules.discover.added", { count: result.rules.length }),
          {
            closeButton: true,
          },
        );
      }
      if (result.warning) {
        toast.warning(
          t("rules.discover.importWarning", { error: result.warning }),
          { closeButton: true },
        );
      } else if (result.rules.length === 0) {
        toast.info(t("rules.discover.alreadyAdded"), { closeButton: true });
      }
    } catch (error) {
      toast.error(
        t("rules.discover.addFailed", {
          error: extractErrorMessage(error),
        }),
        { closeButton: true },
      );
    }
  };

  const importRule = async (name: string) => {
    if (importing.has(name)) return;
    setImporting((prev) => new Set(prev).add(name));
    try {
      await applyImport([name]);
    } finally {
      setImporting((prev) => {
        const next = new Set(prev);
        next.delete(name);
        return next;
      });
    }
  };

  const syncAll = async () => {
    if (syncing) return;
    setSyncing(true);
    try {
      await rulesApi.syncRules();
      await queryClient.invalidateQueries({ queryKey: ruleKeys.all });
      toast.success(t("rules.syncSuccess"), { closeButton: true });
    } catch (error) {
      toast.error(
        t("rules.syncFailed", { error: extractErrorMessage(error) }),
        { closeButton: true },
      );
    } finally {
      setSyncing(false);
    }
  };

  const removeRule = async () => {
    if (!confirmDelete || deleting) return;
    const target = confirmDelete;
    setDeleting(true);
    try {
      await rulesApi.deleteRule(target.id);
      setConfirmDelete(null);
      setSelected((prev) => {
        const next = new Set(prev);
        next.delete(target.id);
        return next;
      });
      await queryClient.invalidateQueries({ queryKey: ruleKeys.all });
      toast.success(t("rules.deleted"));
    } catch (error) {
      toast.error(
        t("rules.deleteFailed", { error: extractErrorMessage(error) }),
      );
    } finally {
      setDeleting(false);
    }
  };

  const showInstalled = (id?: string | null) => {
    setSearchQuery("");
    setStatusFilter("all");
    setView("installed");
    if (id) setHighlightId(id);
  };

  const statusLabel = (value: StatusFilter) => {
    if (value === "enabled") return t("rules.filterEnabled");
    if (value === "disabled") return t("rules.filterDisabled");
    if (value.startsWith("app:")) {
      return t("rules.filterInApp", {
        app: APP_DISPLAY_NAME[value.slice(4) as AppId],
      });
    }
    return t("rules.filterAll");
  };

  const selectionActive = selected.size > 0;
  const busy = pendingCells.size > 0 || syncing;
  const toolbarVisible = !rulesQuery.isLoading && !rulesQuery.isError;

  return (
    <>
      <AppPageHeader
        icon={<ScrollText className="h-[18px] w-[18px]" strokeWidth={2} />}
        title={t("nav.rules")}
        titleExtra={
          <HelpTip title={t("rules.helpTitle")}>{t("rules.helpText")}</HelpTip>
        }
        actions={
          <Button
            type="button"
            variant="solid"
            size="regular"
            onClick={() => setEditing({ mode: "create" })}
          >
            <Plus className="h-4 w-4" strokeWidth={2} />
            {t("rules.add")}
          </Button>
        }
      />

      <div id="main-content" className="flex min-h-0 flex-1 flex-col">
        <div className="shrink-0 px-6 pt-1">
          <PageTabs<RulesView>
            aria-label={t("rules.viewAria")}
            idPrefix="rules-view"
            className="h-11"
            value={view}
            onValueChange={setView}
            items={[
              {
                value: "installed",
                label: rulesQuery.isLoading
                  ? t("rules.tabInstalled")
                  : t("rules.tabInstalledCount", { count: rules.length }),
              },
              { value: "discover", label: t("rules.tabDiscover") },
            ]}
            trailing={
              view === "installed" && toolbarVisible ? (
                <MatrixSearch
                  className="w-[280px] min-w-[160px] shrink"
                  inputId="rules-installed-search"
                  value={searchQuery}
                  onValueChange={setSearchQuery}
                  placeholder={t("rules.searchPlaceholder")}
                  ariaLabel={t("rules.searchAria")}
                  status={
                    normalizedQuery
                      ? t("appMatrix.found", { count: filteredRules.length })
                      : ""
                  }
                />
              ) : view === "discover" ? (
                <MatrixSearch
                  className="w-[280px] min-w-[160px] shrink"
                  inputId="rules-discover-search"
                  value={discoverQuery}
                  onValueChange={setDiscoverQuery}
                  placeholder={t("rules.discover.searchPlaceholder")}
                  ariaLabel={t("rules.discover.searchAria")}
                  status={
                    normalizedDiscoverQuery
                      ? t("appMatrix.found", {
                          count: filteredDiscovered.length,
                        })
                      : ""
                  }
                />
              ) : null
            }
          />
        </div>

        {supportQuery.isError && (
          <div className="mx-6 mt-2 flex shrink-0 items-center justify-between gap-3 rounded-panel border border-border bg-subtle px-4 py-2 text-caption text-fg-2">
            <span className="text-danger-text">
              {t("rules.supportLoadFailed", {
                error: extractErrorMessage(supportQuery.error),
              })}
            </span>
            <Button
              type="button"
              variant="quiet"
              size="compact"
              onClick={() => void supportQuery.refetch()}
            >
              {t("common.retry")}
            </Button>
          </div>
        )}

        {view === "installed" ? (
          <InstalledView
            rules={rules}
            rulesQuery={rulesQuery}
            filteredRules={filteredRules}
            visibleAppIds={visibleAppIds}
            supportAppIds={supportAppIds}
            fails={fails}
            pendingCells={pendingCells}
            selected={selected}
            statusFilter={statusFilter}
            scope={scope}
            normalizedQuery={normalizedQuery}
            toolbarVisible={toolbarVisible}
            selectionActive={selectionActive}
            busy={busy}
            syncing={syncing}
            highlightId={highlightId}
            onHighlightDone={() => setHighlightId(null)}
            onSelectAll={(checked) =>
              setSelected((prev) => {
                const next = new Set(prev);
                for (const rule of filteredRules) {
                  if (checked) next.add(rule.id);
                  else next.delete(rule.id);
                }
                return next;
              })
            }
            onSelect={(id, checked) =>
              setSelected((prev) => {
                const next = new Set(prev);
                if (checked) next.add(id);
                else next.delete(id);
                return next;
              })
            }
            onCell={(rule, app) => {
              const failure = fails[cellKey(rule.id, app)];
              void toggleApp(
                rule,
                app,
                failure ? failure.desired : !appOn(rule.apps, app),
              );
            }}
            onColumnBulk={(app, enabled) =>
              void bulkToggle(scope.rows, app, enabled)
            }
            onSelectionBulk={(app, enabled) =>
              void bulkToggle(
                rules.filter((rule) => selected.has(rule.id)),
                app,
                enabled,
              )
            }
            onClearSelection={() => setSelected(new Set())}
            onFilter={setStatusFilter}
            statusLabel={statusLabel}
            onSync={() => void syncAll()}
            onEdit={(rule) => setEditing({ mode: "edit", rule })}
            onCreate={() => setEditing({ mode: "create" })}
            onGoDiscover={() => setView("discover")}
            onDelete={setConfirmDelete}
            onClearSearch={() => {
              setSearchQuery("");
              setStatusFilter("all");
            }}
            onRetry={() => void rulesQuery.refetch()}
            enabledAppCount={enabledAppCount}
          />
        ) : (
          <DiscoverView
            files={filteredDiscovered}
            total={unaddedDiscovered.length}
            allAdded={allDiscoveredAdded}
            query={discoverQuery}
            isError={discoverQueryResult.isError}
            error={discoverQueryResult.error}
            isFetching={discoverQueryResult.isFetching}
            isLoading={discoverQueryResult.isLoading}
            importing={importing}
            onRefresh={refreshDiscover}
            onRetry={refreshDiscover}
            onImport={(name) => void importRule(name)}
            onShowInstalled={showInstalled}
          />
        )}
      </div>

      {editing && (
        <RuleEditorDialog
          key={editing.mode === "edit" ? editing.rule.id : "create"}
          editing={editing}
          rules={rules}
          onClose={() => setEditing(null)}
          onSaved={(rule) => {
            setEditing(null);
            showInstalled(rule.id);
          }}
        />
      )}

      <V7ConfirmDialog
        open={confirmDelete !== null}
        title={t("rules.delete")}
        body={
          confirmDelete
            ? t("rules.deleteConfirm", { name: confirmDelete.name })
            : ""
        }
        confirmLabel={t("rules.delete")}
        pending={deleting}
        onConfirm={() => void removeRule()}
        onCancel={() => setConfirmDelete(null)}
      />
    </>
  );
}

// ─── Installed ────────────────────────────────────────────────────────────
interface InstalledViewProps {
  rules: Rule[];
  rulesQuery: ReturnType<typeof useRules>;
  filteredRules: Rule[];
  visibleAppIds: AppId[];
  supportAppIds: AppId[];
  fails: Record<string, CellFailure>;
  pendingCells: Set<string>;
  selected: Set<string>;
  statusFilter: StatusFilter;
  scope: ReturnType<typeof resolveBulkScope<Rule>>;
  normalizedQuery: string;
  toolbarVisible: boolean;
  selectionActive: boolean;
  busy: boolean;
  syncing: boolean;
  highlightId: string | null;
  onHighlightDone: () => void;
  onSelectAll: (checked: boolean) => void;
  onSelect: (id: string, checked: boolean) => void;
  onCell: (rule: Rule, app: AppId) => void;
  onColumnBulk: (app: AppId, enabled: boolean) => void;
  onSelectionBulk: (app: AppId, enabled: boolean) => void;
  onClearSelection: () => void;
  onFilter: (value: StatusFilter) => void;
  statusLabel: (value: StatusFilter) => string;
  onSync: () => void;
  onEdit: (rule: Rule) => void;
  onCreate: () => void;
  onGoDiscover: () => void;
  onDelete: (rule: Rule) => void;
  onClearSearch: () => void;
  onRetry: () => void;
  enabledAppCount: (rule: Rule) => number;
}

function InstalledView({
  rules,
  rulesQuery,
  filteredRules,
  visibleAppIds,
  supportAppIds,
  fails,
  pendingCells,
  selected,
  statusFilter,
  scope,
  normalizedQuery,
  toolbarVisible,
  selectionActive,
  busy,
  syncing,
  highlightId,
  onHighlightDone,
  onSelectAll,
  onSelect,
  onCell,
  onColumnBulk,
  onSelectionBulk,
  onClearSelection,
  onFilter,
  statusLabel,
  onSync,
  onEdit,
  onCreate,
  onGoDiscover,
  onDelete,
  onClearSearch,
  onRetry,
  enabledAppCount,
}: InstalledViewProps) {
  const { t } = useTranslation();

  if (rulesQuery.isLoading) {
    return (
      <div className="py-12 text-center text-body text-fg-2">
        {t("common.loading")}
      </div>
    );
  }

  if (rulesQuery.isError) {
    return (
      <div className="flex flex-1 flex-col items-center justify-center gap-2.5 px-10 pb-12 text-center">
        <h2 className="m-0 text-section">{t("rules.loadFailed")}</h2>
        <code className="max-w-[520px] rounded-[8px] bg-subtle px-3 py-2 text-left font-mono text-caption text-fg-2 [overflow-wrap:anywhere]">
          {extractErrorMessage(rulesQuery.error) || String(rulesQuery.error)}
        </code>
        <Button
          type="button"
          variant="neutral"
          size="regular"
          className="mt-1"
          onClick={onRetry}
        >
          {t("common.retry")}
        </Button>
      </div>
    );
  }

  if (rules.length === 0) {
    return (
      <div className="flex flex-1 flex-col items-center justify-center gap-2 px-10 pb-12 text-center">
        <h2 className="m-0 text-section">{t("rules.emptyTitle")}</h2>
        <p className="m-0 max-w-[460px] text-body text-fg-2">
          {t("rules.emptyBody")}
        </p>
        <div className="mt-2 flex gap-2">
          <Button
            type="button"
            variant="neutral"
            size="regular"
            onClick={onCreate}
          >
            {t("rules.newRule")}
          </Button>
          <Button
            type="button"
            variant="neutral"
            size="regular"
            onClick={onGoDiscover}
          >
            {t("rules.goDiscover")}
          </Button>
        </div>
      </div>
    );
  }

  const visibleSelected = filteredRules.filter((rule) =>
    selected.has(rule.id),
  ).length;
  const allSelected =
    filteredRules.length > 0 && visibleSelected === filteredRules.length;

  return (
    <>
      {toolbarVisible && (
        <div className="flex min-h-12 shrink-0 flex-wrap items-center gap-x-2 gap-y-1 px-6 py-2">
          <div className="-ms-2.5 flex min-w-0 items-center gap-0.5">
            <FilterMenu
              label={statusLabel(statusFilter)}
              active={statusFilter !== "all"}
              items={(["all", "enabled", "disabled"] as StatusFilter[]).map(
                (value) => ({
                  key: value,
                  label: statusLabel(value),
                  checked: statusFilter === value,
                  onSelect: () => onFilter(value),
                }),
              )}
            />
          </div>
          <div className="flex-1" />
          {selectionActive ? (
            <div className="flex shrink-0 flex-wrap items-center gap-0.5">
              <span className="shrink-0 whitespace-nowrap pe-1.5 text-body font-medium tabular-nums">
                {t("rules.selected", { count: selected.size })}
              </span>
              <AppMenuButton
                label={t("rules.enableTo")}
                ariaLabel={t("rules.enableToAria")}
                apps={visibleAppIds}
                disabled={busy}
                onPick={(app) => onSelectionBulk(app, true)}
              />
              <AppMenuButton
                label={t("rules.disableIn")}
                ariaLabel={t("rules.disableAria")}
                apps={visibleAppIds}
                disabled={busy}
                onPick={(app) => onSelectionBulk(app, false)}
              />
              <Button
                type="button"
                variant="quiet"
                size="compact"
                className="px-2 text-fg-2"
                onClick={onClearSelection}
              >
                {t("rules.clearSelection")}
              </Button>
            </div>
          ) : (
            <Button
              type="button"
              variant="neutral"
              size="regular"
              className="shrink-0"
              disabled={busy}
              onClick={onSync}
            >
              {syncing ? (
                <Loader2 className="h-4 w-4 animate-spin" />
              ) : (
                <RefreshCw className="h-4 w-4" strokeWidth={2} />
              )}
              {t("rules.menu.sync")}
            </Button>
          )}
        </div>
      )}

      <div className="flex min-h-0 flex-1 flex-col px-6 pb-5">
        <div className="min-h-0 overflow-auto scroll-stable rounded-panel border border-border bg-surface">
          <MatrixColumnHighlight>
            <div style={{ minWidth: matrixMinWidth(visibleAppIds.length) }}>
              <div className="sticky top-0 z-20 flex h-11 items-center border-b border-border bg-subtle pe-2">
                <div className="sticky left-0 z-10 flex min-w-0 flex-1 items-center self-stretch bg-subtle ps-2">
                  <span className="flex w-6 shrink-0 justify-center">
                    <Checkbox
                      aria-label={t("rules.selectAll")}
                      disabled={filteredRules.length === 0}
                      checked={
                        allSelected
                          ? true
                          : visibleSelected > 0
                            ? "indeterminate"
                            : false
                      }
                      onCheckedChange={onSelectAll}
                    />
                  </span>
                  <span className="ms-2 min-w-0 flex-1 truncate text-caption font-semibold text-fg-2">
                    {t("rules.columnName")}
                  </span>
                </div>
                <div className="flex shrink-0">
                  {visibleAppIds.map((app) => (
                    <MatrixColumnHeader
                      key={app}
                      app={app}
                      enabledCount={
                        rules.filter((rule) => appOn(rule.apps, app)).length
                      }
                      totalCount={rules.length}
                      scopeTotal={scope.rows.length}
                      scopeEnabled={
                        scope.rows.filter((rule) => appOn(rule.apps, app))
                          .length
                      }
                      scopeFailed={
                        scope.rows.filter(
                          (rule) => fails[cellKey(rule.id, app)],
                        ).length
                      }
                      scopeKind={scope.kind}
                      noun={t("rules.noun")}
                      disabled={busy}
                      extraAction={{
                        label:
                          statusFilter === `app:${app}`
                            ? t("rules.filterAll")
                            : t("rules.filterInApp", {
                                app: APP_DISPLAY_NAME[app],
                              }),
                        onClick: () =>
                          onFilter(
                            statusFilter === `app:${app}`
                              ? "all"
                              : `app:${app}`,
                          ),
                      }}
                      onEnableRest={() => onColumnBulk(app, true)}
                      onDisableAll={() => onColumnBulk(app, false)}
                    />
                  ))}
                </div>
                <span aria-hidden="true" className="w-10 shrink-0" />
              </div>

              {filteredRules.length === 0 ? (
                <div className="flex flex-col items-center gap-2 px-6 py-10 text-center">
                  <h2 className="m-0 text-section">
                    {normalizedQuery
                      ? t("rules.noSearchResults", {
                          query: normalizedQuery,
                        })
                      : t("rules.noFilterMatch")}
                  </h2>
                  <Button
                    type="button"
                    variant="neutral"
                    size="regular"
                    onClick={onClearSearch}
                  >
                    {normalizedQuery
                      ? t("mcpPage.clearSearch")
                      : t("rules.clearFilter")}
                  </Button>
                </div>
              ) : (
                <ul
                  aria-label={t("rules.listLabel")}
                  className="m-0 list-none p-0"
                >
                  {filteredRules.map((rule, index) => {
                    const highlighted = highlightId === rule.id;
                    const failure = supportAppIds
                      .map((app) => fails[cellKey(rule.id, app)])
                      .find(Boolean);
                    return (
                      <li
                        key={rule.id}
                        id={`rule-row-${rule.id}`}
                        tabIndex={-1}
                        className={cn(
                          "group relative flex h-14 items-center pe-2 outline-none transition-colors duration-150 hover:bg-subtle",
                          index > 0 && "border-t border-border",
                          highlighted &&
                            "isolate before:pointer-events-none before:absolute before:inset-0 before:-z-10 before:bg-warning-soft before:opacity-0 before:content-[''] before:animate-row-flash motion-reduce:before:animate-row-fade",
                        )}
                        onAnimationEnd={(event) => {
                          if (
                            highlighted &&
                            event.target === event.currentTarget
                          ) {
                            onHighlightDone();
                          }
                        }}
                      >
                        <div
                          className={cn(
                            "sticky left-0 z-[1] flex min-w-0 flex-1 items-center self-stretch bg-surface ps-2 transition-colors duration-150 group-hover:bg-subtle",
                            highlighted &&
                              "isolate before:pointer-events-none before:absolute before:inset-0 before:-z-10 before:bg-warning-soft before:opacity-0 before:content-[''] before:animate-row-flash motion-reduce:before:animate-row-fade",
                          )}
                        >
                          <span className="flex w-6 shrink-0 justify-center">
                            <input
                              type="checkbox"
                              className={CHECKBOX_CLASS}
                              aria-label={t("rules.selectRule", {
                                name: rule.name,
                              })}
                              checked={selected.has(rule.id)}
                              onChange={(event) =>
                                onSelect(rule.id, event.target.checked)
                              }
                            />
                          </span>
                          <div className="ms-2 flex min-w-0 flex-1 flex-col pe-3">
                            <div className="flex min-w-0 items-center gap-1.5">
                              <button
                                type="button"
                                onClick={() => onEdit(rule)}
                                className="min-w-0 truncate text-start text-body font-medium hover:text-fg-1 hover:underline hover:underline-offset-[3px]"
                              >
                                {rule.name}
                              </button>
                              {enabledAppCount(rule) === 0 && (
                                <NeutralBadge>
                                  {t("rules.notEnabled")}
                                </NeutralBadge>
                              )}
                              {failure && (
                                <span
                                  className="shrink-0 text-caption text-warning-text"
                                  title={failure.error}
                                >
                                  ⚠
                                </span>
                              )}
                            </div>
                            <span className="truncate text-caption text-fg-2">
                              {rule.preview || t("rules.noContent")}
                            </span>
                          </div>
                        </div>
                        <div className="flex shrink-0">
                          {visibleAppIds.map((app) => {
                            const fail = fails[cellKey(rule.id, app)];
                            const on = appOn(rule.apps, app);
                            return (
                              <MatrixCell
                                key={app}
                                app={app}
                                state={fail ? "fail" : on ? "on" : "off"}
                                disabled={
                                  busy ||
                                  pendingCells.has(cellKey(rule.id, app))
                                }
                                label={t(
                                  `appMatrix.cell.${fail ? "fail" : on ? "on" : "off"}`,
                                  {
                                    name: rule.name,
                                    app: APP_DISPLAY_NAME[app],
                                  },
                                )}
                                onClick={() => onCell(rule, app)}
                              />
                            );
                          })}
                        </div>
                        <div className="flex w-10 shrink-0 justify-end">
                          <DropdownMenu>
                            <DropdownMenuTrigger asChild>
                              <Button
                                type="button"
                                variant="quiet"
                                size="icon-compact"
                                aria-label={t("rules.rowMoreAria", {
                                  name: rule.name,
                                })}
                                disabled={busy}
                              >
                                <MoreHorizontal
                                  className="h-[15px] w-[15px]"
                                  strokeWidth={1.5}
                                />
                              </Button>
                            </DropdownMenuTrigger>
                            <DropdownMenuContent
                              align="end"
                              className="min-w-[180px]"
                            >
                              <DropdownMenuItem onSelect={() => onEdit(rule)}>
                                {t("rules.edit")}
                              </DropdownMenuItem>
                              <DropdownMenuSeparator />
                              <DropdownMenuItem
                                className="text-danger-text focus:text-danger-text"
                                onSelect={() => onDelete(rule)}
                              >
                                {t("rules.delete")}
                              </DropdownMenuItem>
                            </DropdownMenuContent>
                          </DropdownMenu>
                        </div>
                      </li>
                    );
                  })}
                </ul>
              )}
            </div>
          </MatrixColumnHighlight>
        </div>
      </div>
    </>
  );
}

// ─── Discover ─────────────────────────────────────────────────────────────
interface DiscoverViewProps {
  files: DiscoverableRule[];
  total: number;
  allAdded: boolean;
  query: string;
  isError: boolean;
  error: unknown;
  isFetching: boolean;
  isLoading: boolean;
  importing: Set<string>;
  onRefresh: () => void;
  onRetry: () => void;
  onImport: (name: string) => void;
  onShowInstalled: (id?: string | null) => void;
}

function DiscoverView({
  files,
  total,
  allAdded,
  query,
  isError,
  error,
  isFetching,
  isLoading,
  importing,
  onRefresh,
  onRetry,
  onImport,
  onShowInstalled,
}: DiscoverViewProps) {
  const { t } = useTranslation();
  const separator = t("mcpPage.listSeparator");

  return (
    <div className="flex min-h-0 flex-1 flex-col px-6 pb-5">
      <div className="flex min-h-12 shrink-0 items-center gap-2 py-2">
        <span className="shrink-0 text-caption text-fg-3">
          {t("rules.discover.foundCount", { count: total })}
        </span>
        <div className="flex-1" />
        <Button
          type="button"
          variant="neutral"
          size="regular"
          className="shrink-0"
          disabled={isFetching}
          onClick={onRefresh}
        >
          <RefreshCw
            className={cn("h-4 w-4", isFetching && "animate-spin")}
            strokeWidth={2}
          />
          {t("rules.discover.refresh")}
        </Button>
      </div>
      <div className="min-h-0 overflow-auto scroll-stable rounded-panel border border-border bg-surface">
        {isLoading ? (
          <div className="flex items-center justify-center gap-2 py-16 text-body text-fg-2">
            <Loader2 className="h-4 w-4 animate-spin" />
            {t("common.loading")}
          </div>
        ) : isError ? (
          <div className="flex flex-col items-center gap-2 px-6 py-10 text-center">
            <h2 className="m-0 text-section">
              {t("rules.discover.loadFailed")}
            </h2>
            <code className="max-w-[520px] rounded-[8px] bg-subtle px-3 py-2 text-left font-mono text-caption text-fg-2 [overflow-wrap:anywhere]">
              {extractErrorMessage(error) || String(error)}
            </code>
            <Button
              type="button"
              variant="neutral"
              size="regular"
              onClick={onRetry}
            >
              {t("common.retry")}
            </Button>
          </div>
        ) : files.length === 0 ? (
          <div className="flex flex-col items-center gap-2 px-6 py-10 text-center">
            <h2 className="m-0 text-section">
              {allAdded
                ? t("rules.discover.allAddedTitle")
                : query.trim()
                  ? t("rules.discover.noMatch", { query: query.trim() })
                  : t("rules.discover.emptyTitle")}
            </h2>
            {(!query.trim() || allAdded) && (
              <p className="m-0 max-w-[460px] text-body text-fg-2">
                {allAdded
                  ? t("rules.discover.allAddedBody")
                  : t("rules.discover.emptyBody")}
              </p>
            )}
            {!allAdded && (
              <div className="flex flex-wrap justify-center gap-2 pt-1">
                {query.trim() ? (
                  <Button
                    type="button"
                    variant="neutral"
                    size="regular"
                    onClick={() => onShowInstalled()}
                  >
                    {t("rules.tabInstalled")}
                  </Button>
                ) : (
                  <Button
                    type="button"
                    variant="neutral"
                    size="regular"
                    onClick={onRefresh}
                  >
                    {t("rules.discover.refresh")}
                  </Button>
                )}
              </div>
            )}
          </div>
        ) : (
          <ul
            aria-label={t("rules.discover.listLabel")}
            className="m-0 min-w-[520px] list-none p-0"
          >
            {files.map((file, index) => {
              const foundIn = file.apps
                .map((app) => APP_DISPLAY_NAME[app])
                .join(separator);
              const adding = importing.has(file.name);
              return (
                <li
                  key={file.name}
                  className={cn(
                    "flex h-14 items-center gap-3 pe-3 ps-4",
                    index > 0 && "border-t border-border",
                  )}
                >
                  <div className="flex min-w-0 flex-1 flex-col">
                    <div className="flex min-w-0 items-center gap-1.5">
                      <span className="min-w-0 truncate text-body font-medium">
                        {file.name}
                      </span>
                      <span
                        className="min-w-0 shrink truncate font-mono text-caption text-fg-3"
                        title={file.path}
                      >
                        {file.displayPath}
                      </span>
                    </div>
                    <span
                      className="truncate text-caption text-fg-2"
                      title={file.preview || foundIn}
                    >
                      {file.preview ||
                        t("rules.discover.foundIn", { apps: foundIn })}
                    </span>
                  </div>
                  <div
                    className="flex shrink-0 items-center gap-1"
                    title={foundIn}
                  >
                    {file.apps.map((app) => (
                      <AppGlyph
                        key={app}
                        app={app}
                        size={16}
                        badgeClassName="bg-surface"
                      />
                    ))}
                  </div>
                  <div className="flex w-[120px] shrink-0 justify-end">
                    <Button
                      type="button"
                      variant="neutral"
                      size="compact"
                      disabled={adding}
                      onClick={() => onImport(file.name)}
                    >
                      {adding && (
                        <Loader2 className="h-3.5 w-3.5 animate-spin" />
                      )}
                      {adding
                        ? t("rules.discover.adding")
                        : t("rules.discover.add")}
                    </Button>
                  </div>
                </li>
              );
            })}
          </ul>
        )}
      </div>
    </div>
  );
}
