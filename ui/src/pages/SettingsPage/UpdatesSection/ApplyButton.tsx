import { useState, forwardRef, type ReactElement, type Ref } from 'react';
import { Alert, Button, Snackbar } from '@mui/material';
import Dialog from '@mui/material/Dialog';
import DialogActions from '@mui/material/DialogActions';
import DialogContentText from '@mui/material/DialogContentText';
import DialogContent from '@mui/material/DialogContent';
import DialogTitle from '@mui/material/DialogTitle';
import Slide from '@mui/material/Slide';
import { TransitionProps } from '@mui/material/transitions';

import { isConnectionDrop, postUpdatesApply, waitForPoddRestart } from '@api/updates.ts';


const Transition = forwardRef(function Transition(
  props: TransitionProps & {
    children: ReactElement<any, any>;
  },
  ref: Ref<unknown>,
) {
  return <Slide direction="up" ref={ ref } { ...props } />;
});

type ApplyButtonProps = {
  // The app version being offered, shown in the confirmation.
  version: string;
  disabled?: boolean;
  onDone?: () => void;
};

// Applies the offered Tier-2 (app) release. podd installs it, restarts into it
// as a canary and rolls itself back if the new release doesn't come up healthy,
// so the request usually dies in flight — that is success, not failure.
// eslint-disable-next-line react/no-multi-comp
export default function ApplyButton({ version, disabled, onDone }: ApplyButtonProps) {
  const [open, setOpen] = useState(false);
  const [applying, setApplying] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [started, setStarted] = useState(false);

  const apply = () => {
    setOpen(false);
    setApplying(true);
    postUpdatesApply()
      .then(() => {
        setStarted(true);
        onDone?.();
      })
      .catch(async (e: unknown) => {
        // A 409/501 carries the agent's own message (disabled agent,
        // unreachable source, failed verification). A dropped connection is
        // podd restarting into the new release before it could answer: wait
        // for it to come back and report what actually happened.
        if (!isConnectionDrop(e)) {
          console.error(e);
          const message = (e as { response?: { data?: unknown } }).response?.data;
          setError(
            typeof message === 'string' && message.length > 0
              ? message
              : 'Update failed — check the status above.',
          );
          return;
        }
        const report = await waitForPoddRestart();
        const running = report?.updater?.currentVersions.find(v => v.kind === 'app')?.version;
        if (running === version) {
          setStarted(true);
        } else if (report === null) {
          setError('podd has not answered since the restart — reload this page in a minute.');
        } else {
          setError(
            report.updater?.lastError
              ?? `podd is back on ${running ?? 'the previous release'}; the update was rolled back.`,
          );
        }
        onDone?.();
      })
      .finally(() => setApplying(false));
  };

  return (
    <>
      <Button
        variant="contained"
        size="small"
        disabled={ disabled || applying }
        onClick={ () => setOpen(true) }
      >
        { applying ? 'Updating…' : `Update to ${version}` }
      </Button>
      <Dialog
        open={ open }
        slots={ { transition: Transition } }
        keepMounted
        onClose={ () => setOpen(false) }
      >
        <DialogTitle>Update podd to { version }?</DialogTitle>
        <DialogContent>
          <DialogContentText>
            podd downloads and verifies the release, then restarts into it. The new release has
            to pass a health check within a few seconds or podd rolls itself back to the current
            one automatically. The bed keeps its schedule and settings; heating pauses for a few
            seconds while the daemon comes back.
          </DialogContentText>
        </DialogContent>
        <DialogActions>
          <Button onClick={ () => setOpen(false) }>Cancel</Button>
          <Button onClick={ apply }>Update</Button>
        </DialogActions>
      </Dialog>
      <Snackbar
        open={ started }
        autoHideDuration={ 8000 }
        onClose={ () => setStarted(false) }
      >
        <Alert severity="success" onClose={ () => setStarted(false) }>
          Updated to { version }; podd restarted into it
        </Alert>
      </Snackbar>
      <Snackbar
        open={ error !== null }
        autoHideDuration={ 8000 }
        onClose={ () => setError(null) }
      >
        <Alert severity="error" onClose={ () => setError(null) }>
          { error }
        </Alert>
      </Snackbar>
    </>
  );
}
