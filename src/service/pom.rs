use crate::colink_proto::*;
use std::{
    path::Path,
    process::{Command, Stdio},
};
use toml::Value;
use tonic::{Request, Response, Status};
use uuid::Uuid;

impl crate::server::MyService {
    pub async fn _start_protocol_operator(
        &self,
        request: Request<ProtocolOperatorInstance>,
    ) -> Result<Response<ProtocolOperatorInstance>, Status> {
        Self::check_privilege_in(request.metadata(), &["user", "host"])?;
        let privilege = Self::get_key_from_metadata(request.metadata(), "privilege");
        if privilege != "host"
            && Self::get_key_from_metadata(request.metadata(), "user_id")
                != request.get_ref().user_id
        {
            return Err(Status::permission_denied(""));
        }
        let instance_id = Uuid::new_v4();
        let protocol_name: &str = &request.get_ref().protocol_name;
        let file_name = Path::new(&protocol_name).file_name();
        if file_name.is_none() || file_name.unwrap() != protocol_name {
            return Err(Status::invalid_argument("protocol_name is invalid."));
        }
        let colink_home = if std::env::var("COLINK_HOME").is_ok() {
            std::env::var("COLINK_HOME").unwrap()
        } else if std::env::var("HOME").is_ok() {
            std::env::var("HOME").unwrap() + "/.colink"
        } else {
            return Err(Status::not_found("colink home not found."));
        };
        let path = Path::new(&colink_home)
            .join("protocols")
            .join(protocol_name)
            .join("colink.toml");
        if std::fs::metadata(&path).is_err() {
            match fetch_protocol_from_inventory(protocol_name, &colink_home).await {
                Ok(_) => {}
                Err(err) => {
                    return Err(Status::not_found(&format!(
                        "protocol {} not found: {}",
                        protocol_name, err
                    )));
                }
            }
        }
        let toml = std::fs::read_to_string(&path)
            .unwrap()
            .parse::<Value>()
            .unwrap();
        if self.core_uri.is_none() {
            return Err(Status::internal("core_uri not found."));
        }
        let core_addr = self.core_uri.as_ref().unwrap();
        if toml.get("package").is_none() || toml["package"].get("entrypoint").is_none() {
            return Err(Status::not_found("entrypoint not found."));
        }
        let entrypoint = toml["package"]["entrypoint"].as_str();
        if entrypoint.is_none() {
            return Err(Status::not_found("entrypoint not found."));
        }
        let entrypoint = entrypoint.unwrap();
        let user_jwt = self
            ._host_storage_read(&format!("users:{}:user_jwt", request.get_ref().user_id))
            .await?;
        let user_jwt = String::from_utf8(user_jwt).unwrap();
        let process = Command::new("bash")
            .arg("-c")
            .arg(&*entrypoint)
            .current_dir(
                Path::new(&colink_home)
                    .join("protocols")
                    .join(protocol_name),
            )
            .env("COLINK_CORE_ADDR", core_addr)
            .env("COLINK_JWT", user_jwt)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let pid = process.id().to_string();
        self._host_storage_update(
            &format!("protocol_operator_instances:{}:user_id", instance_id),
            request.get_ref().user_id.as_bytes(),
        )
        .await?;
        self._host_storage_update(
            &format!("protocol_operator_instances:{}:pid", instance_id),
            pid.as_bytes(),
        )
        .await?;
        Ok(Response::new(ProtocolOperatorInstance {
            instance_id: instance_id.to_string(),
            ..Default::default()
        }))
    }

    pub async fn _stop_protocol_operator(
        &self,
        request: Request<ProtocolOperatorInstance>,
    ) -> Result<Response<Empty>, Status> {
        Self::check_privilege_in(request.metadata(), &["user", "host"])?;
        let privilege = Self::get_key_from_metadata(request.metadata(), "privilege");
        let user_id = self
            ._host_storage_read(&format!(
                "protocol_operator_instances:{}:user_id",
                request.get_ref().instance_id
            ))
            .await?;
        let user_id = String::from_utf8(user_id).unwrap();
        if privilege != "host"
            && Self::get_key_from_metadata(request.metadata(), "user_id") != user_id
        {
            return Err(Status::permission_denied(""));
        }
        let pid = self
            ._host_storage_read(&format!(
                "protocol_operator_instances:{}:pid",
                request.get_ref().instance_id
            ))
            .await?;
        let pid = String::from_utf8(pid).unwrap();
        Command::new("kill")
            .args(["-9", &pid])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        Ok(Response::new(Empty::default()))
    }
}

const PROTOCOL_INVENTORY: &str =
    "https://raw.githubusercontent.com/CoLearn-Dev/colink-protocol-inventory/main/protocols";
async fn fetch_protocol_from_inventory(
    protocol_name: &str,
    colink_home: &str,
) -> Result<(), String> {
    let url = &format!("{}/{}.toml", PROTOCOL_INVENTORY, protocol_name);
    let http_client = reqwest::Client::new();
    let resp = http_client.get(url).send().await;
    if resp.is_err() || resp.as_ref().unwrap().status() != reqwest::StatusCode::OK {
        return Err(format!(
            "fail to find protocol {} in inventory",
            protocol_name
        ));
    }
    let toml = match resp.unwrap().text().await {
        Ok(toml) => toml.parse::<Value>().unwrap(),
        Err(err) => {
            return Err(err.to_string());
        }
    };
    let path = Path::new(&colink_home)
        .join("protocols")
        .join(protocol_name);
    if toml.get("binary").is_some()
        && toml["binary"]
            .get(&format!(
                "{}-{}",
                std::env::consts::OS,
                std::env::consts::ARCH
            ))
            .is_some()
    {
        if let Some(_binary) = toml["binary"]
            [&format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)]
            .as_table()
        {
            // TODO
            // return Ok(());
        }
    }
    if toml.get("source").is_some() {
        if toml["source"].get("archive").is_some() {
            if let Some(_source) = toml["source"]["archive"].as_table() {
                // TODO
                // return Ok(());
            }
        }
        if toml["source"].get("git").is_some() {
            if let Some(source) = toml["source"]["git"].as_table() {
                fetch_from_git(
                    source["url"].as_str().unwrap(),
                    source["commit"].as_str().unwrap(),
                    path.to_str().unwrap(),
                )
                .await?;
                return Ok(());
            }
        }
    }
    return Err(format!(
        "the inventory file of protocol {} is damaged",
        protocol_name
    ));
}

async fn fetch_from_git(url: &str, commit: &str, path: &str) -> Result<(), String> {
    let git_clone = Command::new("git")
        .args(["clone", "--recursive", url, path])
        .output()
        .unwrap();
    if !git_clone.status.success() {
        return Err(format!("fail to fetch from {}", url));
    }
    let git_checkout = Command::new("git")
        .args(["checkout", commit])
        .current_dir(path)
        .output()
        .unwrap();
    if !git_checkout.status.success() {
        return Err(format!("checkout error: commit {}", commit));
    }
    Ok(())
}
