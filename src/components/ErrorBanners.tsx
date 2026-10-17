import { useTranslation } from "react-i18next";
import { useAppState } from "../state/AppStore";
import { Button } from "./ui";

export function ErrorBanners() {
  const { t } = useTranslation();
  const { state, actionError, dismissActionError } = useAppState();

  if (!state) return null;

  const { vm, share } = state;
  const vmFailing = vm.status === "start_error";
  const shareFailing = share.status === "error";
  const shareDegraded = share.status === "degraded";

  const sourceLabel = actionError
    ? t(
        actionError.kind === "vm"
          ? "error.actionSource.vm"
          : actionError.kind === "share"
            ? "error.actionSource.share"
            : "error.actionSource.generic",
      )
    : "";

  return (
    <div className="bannerStack">
      {vmFailing ? (
        <div className="banner banner--vm" role="alert">
          <div className="bannerHead">
            <span className="bannerCode">VM_ERROR</span>
            <strong>{t("error.vmTitle")}</strong>
          </div>
          <p className="bannerBody">{vm.last_error ?? t("error.noDetail")}</p>
          {vm.consecutive_start_failures > 0 ? (
            <p className="bannerMeta">
              {t("manage.failureCount", { count: vm.consecutive_start_failures })}
            </p>
          ) : null}
        </div>
      ) : null}

      {shareFailing ? (
        <div className="banner banner--share" role="alert">
          <div className="bannerHead">
            <span className="bannerCode">SHARE_ERROR</span>
            <strong>{t("error.shareTitle")}</strong>
          </div>
          <p className="bannerBody">{share.last_error ?? t("error.noDetail")}</p>
          <p className="bannerMeta">{t("error.shareReassurance")}</p>
        </div>
      ) : null}

      {shareDegraded && !shareFailing ? (
        <div className="banner banner--degraded" role="status">
          <div className="bannerHead">
            <strong>{t("error.shareDegradedTitle")}</strong>
          </div>
          <p className="bannerBody">{share.last_error ?? t("error.noDetail")}</p>
          <p className="bannerMeta">{t("error.shareReassurance")}</p>
        </div>
      ) : null}

      {actionError ? (
        <div className="banner banner--action" role="alert">
          <div className="bannerHead">
            <strong>{t("error.actionTitle")}</strong>
            <span className="bannerMetaInline">{sourceLabel}</span>
          </div>
          <p className="bannerBody">{actionError.message}</p>
          <div className="bannerActions">
            <Button onClick={dismissActionError}>{t("error.dismiss")}</Button>
          </div>
        </div>
      ) : null}
    </div>
  );
}
