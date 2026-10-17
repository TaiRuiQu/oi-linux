import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { resetEnvironment } from "../api";
import { toErrorMessage } from "../format";
import { useAppState } from "../state/AppStore";
import { Modal } from "./Modal";
import { Button, CheckRow } from "./ui";

export function ResetDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const { t } = useTranslation();
  const { state, setState } = useAppState();
  const [ack, setAck] = useState(false);
  const [working, setWorking] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (open) {
      setAck(false);
      setError(null);
      setWorking(false);
    }
  }, [open]);

  if (!open) return null;

  const stopped = state?.vm.status === "stopped";

  const confirm = async () => {
    setWorking(true);
    setError(null);
    try {
      const next = await resetEnvironment();
      setState(next);
      onClose();
    } catch (cause) {
      setError(toErrorMessage(cause));
    } finally {
      setWorking(false);
    }
  };

  return (
    <Modal
      title={t("reset.title")}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose} disabled={working}>
            {t("common.cancel")}
          </Button>
          <Button
            variant="danger"
            onClick={() => void confirm()}
            disabled={!ack || !stopped || working}
          >
            {working ? t("reset.busy") : t("reset.confirm")}
          </Button>
        </>
      }
    >
      <p className="warningTitle">{t("reset.warningTitle")}</p>
      <p className="warningBody">{t("reset.guestData")}</p>
      <p className="safeBody">{t("reset.workspaceSafe")}</p>
      {!stopped ? <p className="hint hint--warn">{t("reset.resetMustBeStopped")}</p> : null}
      <CheckRow checked={ack} onChange={setAck}>
        {t("reset.ack")}
      </CheckRow>
      {error ? (
        <p className="hint hint--danger" role="alert">
          {t("reset.failed")} {error}
        </p>
      ) : null}
    </Modal>
  );
}
