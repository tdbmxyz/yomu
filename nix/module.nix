# NixOS module for yomu. Same shape as the chaos module: freeform TOML
# settings, state under /var/lib/yomu, hardened DynamicUser service.
self: {
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.yomu;
  settingsFormat = pkgs.formats.toml {};
  configFile = settingsFormat.generate "yomu.toml" cfg.settings;
in {
  options.services.yomu = {
    enable = lib.mkEnableOption "yomu, the manga library and reader";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.yomu-server;
      defaultText = lib.literalExpression "yomu.packages.\${system}.yomu-server";
      description = "yomu-server package to run.";
    };

    webPackage = lib.mkOption {
      type = lib.types.nullOr lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.yomu-web;
      defaultText = lib.literalExpression "yomu.packages.\${system}.yomu-web";
      description = "Built web frontend served by the server (null to disable).";
    };

    address = lib.mkOption {
      type = lib.types.str;
      default = "0.0.0.0";
      description = "Address to bind to.";
    };

    port = lib.mkOption {
      type = lib.types.port;
      default = 4700;
      description = "Port to listen on.";
    };

    openFirewall = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Open the yomu port in the firewall.";
    };

    sourcesDir = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = ''
        Directory with scan-site definitions (*.toml). Typically a path in
        the system flake so sources are declarative; null keeps the default
        /var/lib/yomu/sources.d for manual management.
      '';
    };

    settings = lib.mkOption {
      type = settingsFormat.type;
      default = {};
      description = ''
        yomu configuration, serialized to yomu.toml. See
        crates/yomu-server/yomu.example.toml. State paths default to
        /var/lib/yomu.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    services.yomu.settings = {
      listen = lib.mkDefault "${cfg.address}:${toString cfg.port}";
      db_path = lib.mkDefault "/var/lib/yomu/yomu.db";
      data_dir = lib.mkDefault "/var/lib/yomu/data";
      sources_dir = lib.mkDefault (
        if cfg.sourcesDir != null
        then cfg.sourcesDir
        else "/var/lib/yomu/sources.d"
      );
      static_dir = lib.mkIf (cfg.webPackage != null) (lib.mkDefault cfg.webPackage);
    };

    systemd.services.yomu = {
      description = "yomu manga server";
      wantedBy = ["multi-user.target"];
      after = ["network-online.target"];
      wants = ["network-online.target"];

      environment.YOMU_CONFIG = configFile;

      serviceConfig = {
        ExecStart = lib.getExe cfg.package;
        DynamicUser = true;
        StateDirectory = "yomu";
        WorkingDirectory = "/var/lib/yomu";
        Restart = "on-failure";
        RestartSec = 5;

        NoNewPrivileges = true;
        PrivateTmp = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        ProtectKernelTunables = true;
        ProtectControlGroups = true;
        RestrictSUIDSGID = true;
        LockPersonality = true;
      };
    };

    networking.firewall.allowedTCPPorts = lib.mkIf cfg.openFirewall [cfg.port];
  };
}
