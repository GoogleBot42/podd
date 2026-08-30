import {
  Box,
  Button,
  Dialog,
  DialogActions,
  Typography,
  useMediaQuery,
  useTheme,
} from '@mui/material';

export interface UnsavedChangesDialogProps {
  open: boolean;
  onDiscard: () => void;
  onCancel: () => void;
}

// Confirms before an in-progress schedule edit is thrown away — mirrors the
// layout of AlarmDisabledDialog (ControlTempPage) for a consistent look.
export default function UnsavedChangesDialog({
  open,
  onDiscard,
  onCancel,
}: UnsavedChangesDialogProps) {
  const theme = useTheme();
  const isSmallScreen = useMediaQuery(theme.breakpoints.down('sm'));

  return (
    <Dialog
      open={ open }
      onClose={ onCancel }
      fullScreen={ isSmallScreen }
      PaperProps={ {
        sx: isSmallScreen
          ? {
            display: 'flex',
            justifyContent: 'center',
            alignItems: 'center',
            textAlign: 'center',
            maxWidth: '85vw',
            maxHeight: '35vh',
            borderRadius: '10px',
            margin: 0,
            p: 4,
          }
          : {
            p: 4,
            width: '50%',
            height: '150px',
          },
      } }
    >
      <Typography variant="h5" textAlign="center">
        Discard unsaved changes?
      </Typography>
      <Typography variant="body2" color="text.secondary" textAlign="center" sx={ { mt: 1 } }>
        You have unsaved schedule changes that will be lost.
      </Typography>

      <DialogActions
        sx={ {
          display: 'flex',
          flexDirection: 'column',
          alignItems: 'center',
          justifyContent: 'center',
        } }
      >
        <Box display="flex" gap={ 1 }>
          <Button variant="contained" size="small" onClick={ onCancel }>
            Keep editing
          </Button>
          <Button variant="contained" color="error" size="small" onClick={ onDiscard }>
            Discard changes
          </Button>
        </Box>
      </DialogActions>
    </Dialog>
  );
}
