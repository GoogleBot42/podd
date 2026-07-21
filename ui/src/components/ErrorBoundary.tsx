import { Component, ErrorInfo as ReactErrorInfo, PropsWithChildren } from 'react';
import { Alert, Typography } from '@mui/material';


type ErrorInfo = {
  error: Error;
  componentStack: string;
}

type ErrorMessageProps = {
  componentName: string;
  errorInfo?: ErrorInfo;
}


const ErrorMessage = ({ componentName, errorInfo }: ErrorMessageProps) => {
  if (errorInfo && import.meta.env.VITE_ENV === 'dev') {
    const errorMessage =
      errorInfo?.error instanceof Error
        ? errorInfo.error.message
        : typeof errorInfo?.error === 'string'
          ? errorInfo.error
          : '';

    return (
      <Alert severity='error'>

        <Typography color='text.secondary' sx={ { fontFamily: 'monospace' } }>
          ERROR: &nbsp;
          { errorMessage }
          <br />
          { errorInfo.componentStack }
        </Typography>
      </Alert>

    );
  } else {
    return (
      <Alert severity='error'>
        { componentName } failed to load
      </Alert>
    );
  }
};

type ErrorBoundaryProps = PropsWithChildren<Pick<ErrorMessageProps, 'componentName'>>;

type ErrorBoundaryState = {
  errorInfo?: ErrorInfo;
  hasError: boolean;
}

// eslint-disable-next-line react/no-multi-comp
export default class ErrorBoundary extends Component<ErrorBoundaryProps, ErrorBoundaryState> {
  state: ErrorBoundaryState = { hasError: false };

  static getDerivedStateFromError(): Partial<ErrorBoundaryState> {
    return { hasError: true };
  }

  componentDidCatch(error: Error, info: ReactErrorInfo) {
    this.setState({ errorInfo: { error, componentStack: info.componentStack ?? '' } });
    console.error(`ErrorBoundary(${this.props.componentName})`, error, info.componentStack);
  }

  render() {
    if (this.state.hasError) {
      return <ErrorMessage componentName={ this.props.componentName } errorInfo={ this.state.errorInfo }/>;
    }
    return this.props.children;
  }
}
