use super::{Install, DISPLAY_NAME, SERVICE_NAME};
use crate::cli::AgentArgs;
use crate::join::system_config_path;
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr::null_mut;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use windows_service::service::{
    ServiceAccess, ServiceAction, ServiceActionType, ServiceControl, ServiceControlAccept,
    ServiceErrorControl, ServiceExitCode, ServiceFailureActions, ServiceFailureResetPeriod,
    ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_service::{define_windows_service, service_dispatcher};

const DESCRIPTION: &str = "Keeps this workstation registered with the Skylatch hub and runs the \
host engine in the active console session.";
const ERROR_FAILED_SERVICE_CONTROLLER_CONNECT: i32 = 1063;
const ERROR_SERVICE_DOES_NOT_EXIST: i32 = 1060;
const ERROR_SERVICE_NOT_ACTIVE: i32 = 1062;
const CONFIG_POLL: Duration = Duration::from_secs(5);
const STOP_TIMEOUT: Duration = Duration::from_secs(30);

static ARGS: Mutex<Option<AgentArgs>> = Mutex::new(None);

define_windows_service!(ffi_service_main, service_main);

fn os_error(err: &windows_service::Error) -> Option<i32> {
    match err {
        windows_service::Error::Winapi(err) => err.raw_os_error(),
        _ => None,
    }
}

/// Hands the process to the service control manager; returns when the service has stopped.
pub fn dispatch(mut agent: AgentArgs) -> Result<(), Box<dyn std::error::Error>> {
    let config = agent.config.get_or_insert_with(system_config_path).clone();
    let logs = config.with_file_name("logs");
    let log = crate::engine_win_session::open_log(&logs.join("agent.log"))?;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_ansi(false)
        .with_writer(Mutex::new(log))
        .init();
    *ARGS.lock().expect("not poisoned") = Some(agent);
    service_dispatcher::start(SERVICE_NAME, ffi_service_main).map_err(|err| {
        if os_error(&err) == Some(ERROR_FAILED_SERVICE_CONTROLLER_CONNECT) {
            format!("`service run` is started by Windows; use `sc start {SERVICE_NAME}`").into()
        } else {
            Box::<dyn std::error::Error>::from(err)
        }
    })
}

fn service_main(_arguments: Vec<OsString>) {
    let Some(agent) = ARGS.lock().expect("not poisoned").take() else {
        return;
    };
    if let Err(err) = run_service(agent) {
        tracing::error!("service failed: {err}");
    }
}

fn status(state: ServiceState, exit_code: u32) -> ServiceStatus {
    ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: if state == ServiceState::Running {
            ServiceControlAccept::STOP
                | ServiceControlAccept::SHUTDOWN
                | ServiceControlAccept::SESSION_CHANGE
        } else {
            ServiceControlAccept::empty()
        },
        exit_code: if exit_code == 0 {
            ServiceExitCode::Win32(0)
        } else {
            ServiceExitCode::ServiceSpecific(exit_code)
        },
        checkpoint: 0,
        wait_hint: if state == ServiceState::StopPending {
            Duration::from_secs(10)
        } else {
            Duration::ZERO
        },
        process_id: None,
    }
}

fn run_service(agent: AgentArgs) -> Result<(), Box<dyn std::error::Error>> {
    let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(false);
    let handler = move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            stop_tx.send_replace(true);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::SessionChange(_) => {
            crate::engine_win_session::console_changed();
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let handle = service_control_handler::register(SERVICE_NAME, handler)?;
    handle.set_service_status(status(ServiceState::Running, 0))?;
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "service started");

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result: Result<(), String> = runtime.block_on(async {
        tokio::select! {
            result = serve(agent) => result,
            _ = stop_rx.wait_for(|stop| *stop) => Ok(()),
        }
    });
    handle.set_service_status(status(ServiceState::StopPending, 0))?;
    runtime.shutdown_timeout(Duration::from_secs(5));
    match &result {
        Ok(()) => tracing::info!("service stopped"),
        Err(err) => tracing::error!("service stopped: {err}"),
    }
    handle.set_service_status(status(ServiceState::Stopped, u32::from(result.is_err())))?;
    Ok(())
}

/// Waits for `join --system` when no configuration exists yet, then runs the agent.
async fn serve(agent: AgentArgs) -> Result<(), String> {
    if agent.hub.is_none() || agent.token.is_none() {
        let config = agent.config.clone().unwrap_or_else(system_config_path);
        if !config.exists() {
            tracing::info!(
                config = %config.display(),
                "waiting for enrollment: run `skylatch-agent join --system <hub-url> --code <code>`"
            );
            while !config.exists() {
                tokio::time::sleep(CONFIG_POLL).await;
            }
        }
    }
    crate::hub::run(agent).await.map_err(|err| err.to_string())
}

fn manager(access: ServiceManagerAccess) -> Result<ServiceManager, Box<dyn std::error::Error>> {
    ServiceManager::local_computer(None::<&str>, access).map_err(|err| {
        format!("cannot open the service control manager ({err}); run from an elevated prompt")
            .into()
    })
}

fn stop_and_wait(
    service: &windows_service::service::Service,
) -> Result<(), Box<dyn std::error::Error>> {
    match service.stop() {
        Ok(_) => {}
        Err(err) if os_error(&err) == Some(ERROR_SERVICE_NOT_ACTIVE) => return Ok(()),
        Err(err) => return Err(err.into()),
    }
    let deadline = Instant::now() + STOP_TIMEOUT;
    while service.query_status()?.current_state != ServiceState::Stopped {
        if Instant::now() >= deadline {
            return Err(format!("{SERVICE_NAME} did not stop within {STOP_TIMEOUT:?}").into());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Ok(())
}

pub fn install(install: &Install) -> Result<(), Box<dyn std::error::Error>> {
    let config = std::path::absolute(install.config.clone().unwrap_or_else(system_config_path))?;
    let info = ServiceInfo {
        name: SERVICE_NAME.into(),
        display_name: DISPLAY_NAME.into(),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: std::env::current_exe()?,
        launch_arguments: install.launch_arguments(&config)?,
        dependencies: vec![],
        account_name: None,
        account_password: None,
    };
    let manager = manager(ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE)?;
    let access = ServiceAccess::QUERY_STATUS
        | ServiceAccess::START
        | ServiceAccess::STOP
        | ServiceAccess::CHANGE_CONFIG;
    let (service, verb) = match manager.open_service(SERVICE_NAME, access) {
        Ok(service) => {
            service.change_config(&info)?;
            (service, "updated")
        }
        Err(err) if os_error(&err) == Some(ERROR_SERVICE_DOES_NOT_EXIST) => {
            (manager.create_service(&info, access)?, "installed")
        }
        Err(err) => return Err(err.into()),
    };
    service.set_description(DESCRIPTION)?;
    let restart = |secs| ServiceAction {
        action_type: ServiceActionType::Restart,
        delay: Duration::from_secs(secs),
    };
    service.update_failure_actions(ServiceFailureActions {
        reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(24 * 60 * 60)),
        reboot_msg: None,
        command: None,
        actions: Some(vec![restart(5), restart(10), restart(30)]),
    })?;
    if let Err(err) = service.set_failure_actions_on_non_crash_failures(true) {
        eprintln!("warning: the service restarts only after crashes, not on error exits: {err}");
    }
    println!("{SERVICE_NAME} {verb}; configuration: {}", config.display());
    if install.start {
        stop_and_wait(&service)?;
        service.start::<&OsStr>(&[])?;
        println!("{SERVICE_NAME} started");
    }
    if !config.exists() {
        println!(
            "Enroll it from an elevated prompt: skylatch-agent join --system <hub-url> --code <code>"
        );
    }
    Ok(())
}

pub fn uninstall() -> Result<(), Box<dyn std::error::Error>> {
    let manager = manager(ServiceManagerAccess::CONNECT)?;
    let access = ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE;
    let service = match manager.open_service(SERVICE_NAME, access) {
        Ok(service) => service,
        Err(err) if os_error(&err) == Some(ERROR_SERVICE_DOES_NOT_EXIST) => {
            println!("{SERVICE_NAME} is not installed");
            return Ok(());
        }
        Err(err) => return Err(err.into()),
    };
    stop_and_wait(&service)?;
    service.delete()?;
    println!("{SERVICE_NAME} removed");
    Ok(())
}

/// Gives `dir` and everything inside it a protected ACL: full control for SYSTEM and
/// Administrators, nothing for anybody else, owned by Administrators.
pub(crate) fn restrict_to_admins(dir: &Path) -> io::Result<()> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SetNamedSecurityInfoW,
        SDDL_REVISION_1, SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::{
        GetSecurityDescriptorDacl, GetSecurityDescriptorOwner, DACL_SECURITY_INFORMATION,
        OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
    };

    let wide = |s: &OsStr| s.encode_wide().chain(Some(0)).collect::<Vec<u16>>();
    let sddl = wide(OsStr::new("O:BAD:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)"));
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: NUL-terminated SDDL and an out-parameter; freed with LocalFree below.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let result = (|| {
        let (mut present, mut defaulted) = (0, 0);
        let mut dacl = null_mut();
        let mut owner = null_mut();
        // SAFETY: `descriptor` is valid; the out-pointers point into it.
        unsafe {
            if GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted) == 0
                || GetSecurityDescriptorOwner(descriptor, &mut owner, &mut defaulted) == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        let path = wide(dir.as_os_str());
        // SAFETY: NUL-terminated path; owner and DACL come from a valid descriptor.
        let code = unsafe {
            SetNamedSecurityInfoW(
                path.as_ptr(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION
                    | DACL_SECURITY_INFORMATION
                    | PROTECTED_DACL_SECURITY_INFORMATION,
                owner,
                null_mut(),
                dacl,
                null_mut(),
            )
        };
        if code == 0 {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(code as i32))
        }
    })();
    // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
    unsafe { LocalFree(descriptor) };
    result
}
