import Alert from '@mui/material/Alert';
import CircularProgress from '@mui/material/CircularProgress';

export default function AnalyzeSleepNotification() {
  return (
    <Alert severity="info">
      Analyzing sleep, results will appear in the data tab
      &nbsp;
      <CircularProgress size={ 15 } sx={ {} }/>
    </Alert>
  );
}
