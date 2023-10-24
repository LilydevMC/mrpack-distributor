use std::{env, fs};
use std::path::Path;
use std::process::Command;
use anyhow::anyhow;
use chrono::Utc;
use clap::{Parser, Subcommand, command};
use serenity::model::channel::Embed;
use serenity::model::webhook::Webhook;

use crate::{
    github::*,
    models::{
        project_type::{
            modpack::config::ModpackConfig,
            mc_mod::config::ModConfig
        },
        modrinth::{
            project::ProjectResponse,
            ModrinthUrl
        }
    },
    modrinth::{
        create_modrinth_release
    },
    pack::*,
    util::*,
    version::*
};

mod github;
mod models;
mod modrinth;
mod pack;
mod util;
mod version;


#[derive(Debug, Parser)]
#[command(
    name = "peony",
    author,
    version,
    about
)]
struct CliArgs {
    #[command(subcommand)]
    commands: Commands
}

#[derive(Debug, Subcommand)]
enum Commands {
    #[command(about = "Export and upload Packwiz modpack")]
    Modpack {
        #[clap(long, short, help = "Whether or not to send Discord webhook")]
        discord: bool,
        #[clap(long, short, help = "Custom version number")]
        version: Option<String>
    },
    #[command(about = "Build and upload Fabric/Quilt mc_mod")]
    Mod {
        #[clap(long, short, help = "Whether or not to send Discord webhook")]
        discord: bool,
        #[clap(long, short, help = "Args to pass to Gradle", default_value = "build")]
        gradle_args: String
    }
}

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    match dotenvy::dotenv() {
        Ok(_) => (),
        Err(_) => ()
    };

    let args = CliArgs::parse();

    match args.commands {
        Commands::Modpack { discord, version } => {

            match which::which("packwiz") {
                Ok(_) => (),
                Err(err) => return Err(anyhow!("Failed to find packwiz executable: {}", err))
            }

            if !Path::new("mrpack.toml").exists() {
                return Err(anyhow!("Failed to find `mrpack.toml` file."))
            }

            let config_file = match fs::read_to_string("mrpack.toml") {
                Ok(content_string) => {
                    let parsed_config: ModpackConfig = match toml::from_str(&*content_string) {
                        Ok(config) => config,
                        Err(err) => return Err(anyhow!(
                            "Failed to parse config file: {}", err
                        ))
                    };
                    parsed_config
                },
                Err(err) => return Err(anyhow!(
                    "Failed to read config file: {}", err
                ))
            };

            let mut pack_file = match get_pack_file() {
                Ok(file) => file,
                Err(err) => return Err(err)
            };

            let tmp_info = match create_temp() {
                Ok(info) => info,
                Err(err) => return Err(err)
            };

            match version {
                Some(ver) => {
                    let mut new_file_contents = pack_file.clone();
                    new_file_contents.version = ver;
                    let file_contents_string = match toml::to_string(
                        &new_file_contents
                    ) {
                        Ok(file) => file,
                        Err(err) => return Err(anyhow!(
                            "Failed to parse new pack data to toml: {}", err
                        ))
                    };

                    pack_file = new_file_contents;

                    match write_pack_file(&tmp_info.dir_path, file_contents_string) {
                        Ok(_) => (),
                        Err(err) => return Err(err)
                    }
                },
                None => ()
            }

            match Command::new("packwiz")
                .arg("mr")
                .arg("export")
                .current_dir(&tmp_info.dir_path).output() {
                Ok(_) => (),
                Err(err) => return Err(anyhow!(
                    "Failed to export with packwiz: {}", err
                ))
            }

            let output_file_info = match get_output_file(&tmp_info) {
                Ok(file_info) => file_info,
                Err(err) => return Err(err)
            };

            let version_info = match get_version_info(
                &config_file,
                &pack_file,
                &output_file_info
            ) {
                Ok(info) => info,
                Err(err) => return Err(err)
            };


            // Changelog

            let changelog_markdown = match generate_changelog(
                &config_file
            ).await {
                Ok(changelog) => changelog,
                Err(err) => return Err(err)
            };

            // GitHub Release

            match create_github_release(
                &config_file,
                &pack_file,
                &output_file_info,
                &version_info,
                &changelog_markdown
            ).await {
                Ok(_) => (),
                Err(err) => println!("Failed to create GitHub release: {}", err)
            }


            // Modrinth Release

            let modrinth_token = match env::var("MODRINTH_TOKEN") {
                Ok(token) => token,
                Err(err) => return Err(anyhow!(
                    "Failed to get `MODRINTH_TOKEN`: {}", err
                ))
            };

            let modrinth_url = ModrinthUrl::new(
                &config_file.modrinth
                );

            match create_modrinth_release(
                &config_file,
                &pack_file,
                &output_file_info,
                &version_info,
                &changelog_markdown,
                modrinth_token.clone(),
                &modrinth_url
            ).await {
                Ok(_) => (),
                Err(err) => println!("{}", err)
            }

            // Send Discord webhook

            if discord {
                let discord_config = match config_file.discord {
                    Some(config) => config,
                    None => return Err(anyhow!(
                        "Failed to get Discord config"
                    ))
                };

                let modrinth_project = match reqwest::Client::new()
                    .get(format!(
                        "{}/project/{}",
                        modrinth_url.labrinth,
                        config_file.modrinth.project_id
                    ))
                    .header("Authorization", modrinth_token)
                    .send().await {
                        Ok(res) => {
                            match res.json::<ProjectResponse>().await {
                                Ok(json) => json,
                                Err(err) => return Err(anyhow!(
                                    "Error parsing response from get project: {}\n\
                                    Make sure this project is not a draft!",
                                    err.to_string()
                                ))
                            }
                        },
                        Err(err) => return Err(anyhow!(
                            "Error getting project from project id: {}",
                            err
                        ))
                };

                let description = format!("\
                **New release!**\n\n\
                {} [GitHub](https://github.com/{}/{}/releases/latest)\n\
                {} [Modrinth]({}/modpack/{})\n\n\
                {}
                ",
                    discord_config.github_emoji_id,
                    config_file.github.repo_owner,
                    config_file.github.repo_name,
                    discord_config.modrinth_emoji_id,
                    modrinth_url.knossos,
                    modrinth_project.slug,
                    changelog_markdown
                );

                let embed_color = match discord_config.embed_color {
                    Some(color) => color as i32,
                    None => match modrinth_project.color {
                        Some(color) => color,
                        None => 0x1e1f22
                    }
                };

                let release_time = Utc::now().format("%b, %d %Y %r");

                let embed = Embed::fake(|e| {
                    e.title(format!("{} {}", discord_config.title_emoji, version_info.version_name))
                        .color(embed_color)
                        .description(description)
                        .image(discord_config.embed_image_url)
                        .footer(|f| {
                            f.text(format!(
                                "{} | {} UTC",
                                modrinth_project.project_type.formatted(),
                                release_time
                            ))
                        })
                });

                let http = serenity::http::Http::new("token");
                let url = match env::var("WEBHOOK_URL") {
                    Ok(url) => url,
                    Err(err) => return Err(anyhow!(
                        "Failed to get webhook url: {}", err
                    ))
                };

                let webhook = Webhook::from_url(&http, &*url).await?;

                webhook.execute(&http, true, |w| {
                    w
                        .content(discord_config.discord_ping_role)
                        .embeds(vec![embed])
                }).await?;

            }


            clean_up(&tmp_info.dir_path)?
        },
        Commands::Mod { discord, gradle_args } => {
            match which::which("java") {
                Ok(_) => (),
                Err(err) => return Err(anyhow!("Failed to find Java executable: {}", err))
            }

            let mut gradlew_path: &Path;

            if env::consts::OS == "windows" {
                gradlew_path = Path::new(".\\gradlew.bat");
            } else {
                gradlew_path = Path::new("./gradlew");
            }

            if !Path::new(gradlew_path).exists() {
                return Err(anyhow!("Failed to find gradle script at `{:?}`", gradlew_path))
            }

            if !Path::new("peony_mod.toml").exists() {
                return Err(anyhow!("Failed to find `peony_mod.toml` file"))
            }


            let config_file = match fs::read_to_string("peony_mod.toml") {
                Ok(content_string) => {
                    let parsed_config: ModConfig = match toml::from_str(&*content_string) {
                        Ok(config) => config,
                        Err(err) => return Err(anyhow!(
                            "Failed to parse config file: {}", err
                        ))
                    };
                    parsed_config
                },
                Err(err) => return Err(anyhow!(
                    "Failed to read config file: {}", err
                ))
            };

            let tmp_info = match create_temp() {
                Ok(info) => {
                    info
                },
                Err(err) => {
                    return Err(anyhow!("Failed to create temporary directory: {}", err))
                }
            };

            let mut gradle_command = Command::new(gradlew_path);

            let gradle_command = gradle_command
                .arg(gradle_args)
                .current_dir(&tmp_info.dir_path);

            let mut gradle_child = match gradle_command.spawn() {
                Ok(child) => child,
                Err(err) => return Err(anyhow!(
                    "Failed to build with Gradle: {}", err
                ))
            };

            gradle_child.wait().unwrap();


            clean_up(&tmp_info.dir_path)?

        }
    }
    Ok(())
}
