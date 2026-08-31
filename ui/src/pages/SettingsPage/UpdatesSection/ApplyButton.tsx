import { useState, forwardRef, type ReactElement, type Ref } from 'react';
import { Alert, Button, Snackbar } from '@mui/material';
import Dialog from '@mui/material/Dialog';
import DialogActions from '@mui/material/DialogActions';
import DialogContentText from '@mui/material/DialogContentText';
import DialogContent from '@mui/material/DialogContent';
import DialogTitle from '@mui/material/DialogTitle';
import Slide from '@mui/material/Slide';
import { TransitionProps } from '@mui/material/transitions';

import { postUpdatesApply } from '@api/updates.ts';


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
      .catch((e: unknown) => {
        console.error(e);
        // A 409 carries the agent's own message (disabled agent, unreachable
        // source, failed verification); anything else is most likely podd
        // restarting into the new release before it could answer.
        const message = (e as { response?: { data?: unknown } }).response?.data;
        setError(
          typeof message === 'string' && message.length > 0
            ? message
            : 'Update failed (or podd restarted before answering) — check the status above.',
        );
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
          Installing { version }; podd is restarting into it
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
