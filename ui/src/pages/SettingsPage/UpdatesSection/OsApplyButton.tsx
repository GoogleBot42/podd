import { useState } from 'react';
import { Alert, Button, Snackbar } from '@mui/material';
import Dialog from '@mui/material/Dialog';
import DialogActions from '@mui/material/DialogActions';
import DialogContentText from '@mui/material/DialogContentText';
import DialogContent from '@mui/material/DialogContent';
import DialogTitle from '@mui/material/DialogTitle';

import { postUpdatesApplyOs } from '@api/updates.ts';


type OsApplyButtonProps = {
  // The OS version being offered, shown in the confirmation.
  version: string;
  // PODD_UPDATER_OS_DRY_RUN on the daemon: the apply verifies and logs only.
  dryRun: boolean;
  disabled?: boolean;
  onDone?: () => void;
};

// Applies the offered Tier-1 (OS) image: podd verifies it, writes the inactive
// A/B slot, reads it back and arms U-Boot's rollback. Unlike the app tier
// nothing restarts — the new slot is tried on the next reboot, which podd
// never does on its own. The confirmation states whether the daemon is armed
// or in OS dry-run, because that decides whether anything gets written.
export default function OsApplyButton({ version, dryRun, disabled, onDone }: OsApplyButtonProps) {
  const [open, setOpen] = useState(false);
  const [applying, setApplying] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<string | null>(null);

  const apply = () => {
    setOpen(false);
    setApplying(true);
    postUpdatesApplyOs()
      .then(status => {
        setDone(status.lastApplied ?? `OS ${version} applied`);
      })
      .catch((e: unknown) => {
        console.error(e);
        // A 400/409 carries the daemon's own message (dry-run fallback
        // writer, failed verification, unmet min_app, disabled agent).
        const message = (e as { response?: { data?: unknown } }).response?.data;
        setError(
          typeof message === 'string' && message.length > 0
            ? message
            : 'OS update failed — check the status above.',
        );
      })
      .finally(() => {
        setApplying(false);
        onDone?.();
      });
  };

  return (
    <>
      <Button
        variant="outlined"
        color="warning"
        size="small"
        disabled={ disabled || applying }
        onClick={ () => setOpen(true) }
      >
        { applying ? 'Writing OS…' : `OS update to ${version}` }
      </Button>
      <Dialog open={ open } onClose={ () => setOpen(false) }>
        <DialogTitle>Apply OS image { version }?</DialogTitle>
        <DialogContent>
          {
            dryRun ? (
              <Alert severity="info" sx={ { mb: 2 } }>
                OS dry-run is on (<code>PODD_UPDATER_OS_DRY_RUN</code>): podd downloads and
                verifies the image and logs the slot plan, but writes nothing.
              </Alert>
            ) : (
              <Alert severity="warning" sx={ { mb: 2 } }>
                Armed: podd will overwrite the inactive boot slot on the SD card and arm the
                bootloader to try it on the next boot. OS over-the-air updates have not finished
                their hardware verification yet.
              </Alert>
            )
          }
          <DialogContentText>
            Downloading and writing the image takes a few minutes; the bed keeps running
            meanwhile. Nothing changes until you reboot the Pod — podd never reboots on its own.
            If the new slot fails to boot three times, the bootloader goes back to the current
            one by itself; once it boots healthy, podd confirms it.
          </DialogContentText>
        </DialogContent>
        <DialogActions>
          <Button onClick={ () => setOpen(false) }>Cancel</Button>
          <Button color="warning" onClick={ apply }>
            { dryRun ? 'Run dry-run' : 'Write OS image' }
          </Button>
        </DialogActions>
      </Dialog>
      <Snackbar
        open={ done !== null }
        autoHideDuration={ 10000 }
        onClose={ () => setDone(null) }
      >
        <Alert severity="success" onClose={ () => setDone(null) }>
          { done }
        </Alert>
      </Snackbar>
      <Snackbar
        open={ error !== null }
        autoHideDuration={ 10000 }
        onClose={ () => setError(null) }
      >
        <Alert severity="error" onClose={ () => setError(null) }>
          { error }
        </Alert>
      </Snackbar>
    </>
  );
}
