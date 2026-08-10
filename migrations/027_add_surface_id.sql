ALTER TABLE workflows ADD COLUMN surface_id UUID REFERENCES surfaces(id) ON DELETE SET NULL;
ALTER TABLE workflow_templates ADD COLUMN surface_id UUID REFERENCES surfaces(id) ON DELETE SET NULL;

CREATE INDEX idx_workflows_surface ON workflows(surface_id);
CREATE INDEX idx_workflow_templates_surface ON workflow_templates(surface_id);
