-- Migration: Add footer settings to tenants table
ALTER TABLE tenants ADD COLUMN IF NOT EXISTS footer_year VARCHAR(4) DEFAULT '2026';
ALTER TABLE tenants ADD COLUMN IF NOT EXISTS footer_company VARCHAR(255) DEFAULT 'SwiftSoftware';
