import { useQuery } from "@tanstack/react-query";
import { rulesApi } from "@/lib/api";

export const ruleKeys = {
  all: ["rules"] as const,
  list: () => ["rules", "list"] as const,
  support: () => ["rules", "support"] as const,
  discover: () => ["rules", "discover"] as const,
};

export function useRules() {
  return useQuery({
    queryKey: ruleKeys.list(),
    queryFn: () => rulesApi.getRules(),
    staleTime: 10_000,
  });
}

export function useRuleAppSupport() {
  return useQuery({
    queryKey: ruleKeys.support(),
    queryFn: () => rulesApi.getAppSupport(),
    staleTime: Number.POSITIVE_INFINITY,
  });
}

export function useDiscoverableRules(enabled = true) {
  return useQuery({
    queryKey: ruleKeys.discover(),
    queryFn: () => rulesApi.discoverRules(),
    staleTime: 10_000,
    enabled,
  });
}
